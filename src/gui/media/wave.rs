//! The audio window's waveform (spec §7): one decode of the whole file into min/max peaks per
//! channel — two lanes for stereo (more channels are mixed down to two), one for mono — at a
//! fixed resolution of the source, never the PCM itself. The peaks are handed over as they
//! fill, about every [`HANDOVER_EVERY`] (a streamed file arrives over time), with how far into
//! the source they reach ([`Source::read`]), and the timeline on screen is drawn from them
//! through the edit list, so an edit redraws it at once, with no decode. A remote file is read
//! through its `bulk` URL (06 §7.4): a Bulk reader, which never evicts playback's blocks and
//! yields to its demand; a decode whose stream was cut short is a failure, never a complete
//! waveform. The decode stops when its window goes ([`Decode`]); one that follows a preview
//! goes on from where the preview's stopped ([`resume`], 02 §4.4).

use super::child::{Ending, Slot};
use super::timeline::{Clip, Timeline};
use crate::ffmpeg::{Input, Tools};
use crate::stream::http::Kind;
use std::io::Read;
use std::process::{Command, Stdio};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

/// Decode rate for the peaks, per channel.
pub const RATE: f64 = 8000.0;
/// Peaks per second of source, up to [`MAX_BINS`] (a longer file gets wider bins).
const BINS_PER_S: f64 = 100.0;
/// At most this many bins per lane: 2 lanes × 2^19 × 8 bytes = 8 MiB, whatever the length.
pub const MAX_BINS: usize = 1 << 19;

/// Min/max per column over a stream of samples whose total is estimated up front.
pub struct Peaks {
    per_bin: usize,
    pub bins: Vec<[f32; 2]>,
    seen: usize,
}

impl Peaks {
    pub fn new(expected_samples: usize, bins: usize) -> Self {
        Self {
            per_bin: expected_samples.div_ceil(bins.max(1)).max(1),
            bins: vec![[0.0, 0.0]; bins.max(1)],
            seen: 0,
        }
    }

    pub fn push(&mut self, v: f32) {
        let k = (self.seen / self.per_bin).min(self.bins.len() - 1);
        let bin = &mut self.bins[k];
        bin[0] = bin[0].min(v);
        bin[1] = bin[1].max(v);
        self.seen += 1;
    }
}

/// The source's peaks: `lanes[c][k]` = min/max of channel `c` over bin `k`, each bin
/// `bin_secs` of the source; bins not reached yet are empty (`[0, 0]`).
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub bin_secs: f64,
    pub lanes: Vec<Vec<[f32; 2]>>,
    /// Counts the hand-overs (the window redraws on a new one).
    pub version: u64,
    /// The decode read the whole file and ended well.
    pub complete: bool,
    /// How many seconds of the source the peaks reach: what lies past it is not read yet.
    pub read: f64,
    /// The decode failed (what it said): the peaks are what it read before, never complete.
    pub error: Option<String>,
}

/// Filled in by the decoder thread as it reads, then once more when it is done.
pub type Shared = Arc<Mutex<Option<Source>>>;

/// Hand-overs while decoding: the peaks so far, at least this many times over the file, and
/// at least every [`HANDOVER_EVERY`] (06 §7.4: a streamed file fills as its bytes come).
const HANDOVERS: usize = 32;
pub const HANDOVER_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// How many bins a `duration`-second source gets.
pub fn bins_for(duration: f64) -> usize {
    ((duration * BINS_PER_S).ceil() as usize).clamp(1, MAX_BINS)
}

/// Reads `input` (interleaved f32le, `lanes` channels at [`RATE`]) into peaks, handing the
/// peaks so far to `publish` every 1/[`HANDOVERS`] of the expected samples, and after any read
/// that ends [`HANDOVER_EVERY`] or more after the last hand-over with samples it has not handed
/// over (a slow stream gives few samples a read). Returns the peaks at the end of its input, or the read error that stopped it (the
/// end of input is not an error; a sample cut short at it is dropped). Nothing it publishes
/// is complete: whether the decode ended well is the caller's to say ([`start`]).
pub fn read_peaks(
    input: &mut dyn Read,
    lanes: usize,
    duration: f64,
    publish: &mut dyn FnMut(Source),
) -> std::io::Result<Source> {
    read_onto(input, lanes, duration, None, publish)
}

/// [`read_peaks`], going on from peaks read before (`onto`: the peaks of the source's first
/// seconds and how many seconds they cover — a preview's, 02 §4.4): `input` is the sound from
/// there on, and its samples land where they are in the source. The peaks before stay as they
/// are (the bin the two meet in takes both).
pub fn read_onto(
    input: &mut dyn Read,
    lanes: usize,
    duration: f64,
    onto: Option<(&Source, f64)>,
    publish: &mut dyn FnMut(Source),
) -> std::io::Result<Source> {
    let lanes = lanes.max(1);
    let bins = bins_for(duration);
    let expected = (duration * RATE).ceil() as usize;
    let mut peaks: Vec<Peaks> = (0..lanes).map(|_| Peaks::new(expected, bins)).collect();
    let mut version = 0;
    if let Some((before, secs)) = onto {
        for (lane, peaks) in peaks.iter_mut().enumerate() {
            if let Some(read) = before.lanes.get(lane).filter(|l| l.len() == bins) {
                peaks.bins.clone_from(read);
            }
            peaks.seen = (secs * RATE).round().max(0.0) as usize;
        }
        version = before.version;
    }
    let step = (expected * lanes / HANDOVERS).max(1);
    let per_bin = peaks[0].per_bin;
    let mut snapshot = |peaks: &[Peaks]| {
        version += 1;
        Source {
            bin_secs: per_bin as f64 / RATE,
            lanes: peaks.iter().map(|p| p.bins.clone()).collect(),
            version,
            complete: false,
            read: peaks[0].seen as f64 / RATE,
            error: None,
        }
    };
    // What one read of the decoder's output gives, and a sample it cut in two.
    let mut buf = vec![0u8; 64 * 1024];
    let (mut part, mut have) = ([0u8; 4], 0usize);
    let (mut c, mut n, mut handed_n) = (0, 0usize, 0usize);
    let mut handed = std::time::Instant::now();
    loop {
        let got = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(got) => got,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        for &byte in &buf[..got] {
            part[have] = byte;
            have += 1;
            if have < 4 {
                continue;
            }
            have = 0;
            peaks[c].push(f32::from_le_bytes(part));
            c = (c + 1) % lanes;
            n += 1;
            if n % step == 0 {
                publish(snapshot(&peaks));
                (handed, handed_n) = (std::time::Instant::now(), n);
            }
        }
        // After every read, however little it gave: a slow stream fills by the time — with
        // something new only.
        if n > handed_n && handed.elapsed() >= HANDOVER_EVERY {
            publish(snapshot(&peaks));
            (handed, handed_n) = (std::time::Instant::now(), n);
        }
    }
    Ok(snapshot(&peaks))
}

/// The ffmpeg run that decodes `input`'s sound into `lanes` lanes of f32le at [`RATE`] on its
/// stdout: from `from` seconds of the source, `len` seconds of it (`None`: to its end). Not
/// started.
pub fn command(tools: &Tools, input: &Input, lanes: usize, from: f64, len: Option<f64>) -> Command {
    let mut cmd = Command::new(&tools.ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    if from > 0.0 {
        cmd.arg("-ss").arg(format!("{from:.6}"));
    }
    if let Some(len) = len {
        cmd.arg("-t").arg(format!("{len:.6}"));
    }
    cmd.args(input.args()).arg("-vn");
    if lanes >= 2 {
        // The source's first two channels as they are — of a sound in more than two, not a
        // mix of them all (its lanes are named after those two channels).
        cmd.args(["-af", "pan=stereo|c0=c0|c1=c1"]);
    } else {
        cmd.args(["-ac", "1"]);
    }
    cmd.arg("-ar")
        .arg(format!("{RATE}"))
        .args(["-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// A waveform decode and the peaks it fills. Dropped — its window went, or plays another file
/// — the decode stops: its ffmpeg is killed (the decoder's thread reaps it) and nothing more is
/// handed over.
pub struct Decode {
    pub peaks: Shared,
    /// The running ffmpeg: in its slot until it has exited, so the drop can always reach it.
    child: Arc<Slot>,
    /// Tests: the decoder has read its ffmpeg's output to the end and waits for its exit.
    #[cfg(test)]
    waiting: Arc<AtomicBool>,
}

impl Decode {
    /// Peaks that need no decode (a preview read the whole file's, 02 §4.4).
    pub fn done(peaks: Source) -> Self {
        Self {
            peaks: Arc::new(Mutex::new(Some(peaks))),
            child: Arc::default(),
            #[cfg(test)]
            waiting: Arc::default(),
        }
    }
}

#[cfg(test)]
impl Decode {
    /// Tests: the process id of the ffmpeg that runs now.
    pub(crate) fn pid(&self) -> Option<u32> {
        self.child.pid()
    }

    /// Tests: its output was read to the end; its exit is waited for.
    pub(crate) fn waiting(&self) -> bool {
        self.waiting.load(SeqCst)
    }
}

impl Drop for Decode {
    /// Only signals (it runs on the window's frame): the decoder's thread reaps.
    fn drop(&mut self) {
        self.child.stop();
    }
}

/// Tests: every waveform decode started on a file — where in the source it started and how many
/// seconds it was to read — in order.
#[cfg(test)]
static STARTS: Mutex<Vec<(std::path::PathBuf, f64, Option<f64>)>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn note_start(input: &Input, from: f64, len: Option<f64>) {
    if let Some(path) = input.local() {
        let mut starts = STARTS.lock().unwrap_or_else(|p| p.into_inner());
        starts.push((path.to_path_buf(), from, len));
    }
}

/// Tests: the waveform decodes started on `file`, `(from, len)` in seconds, in order.
#[cfg(test)]
pub(crate) fn starts(file: &std::path::Path) -> Vec<(f64, Option<f64>)> {
    let starts = STARTS.lock().unwrap_or_else(|p| p.into_inner());
    starts
        .iter()
        .filter(|(p, _, _)| p == file)
        .map(|&(_, from, len)| (from, len))
        .collect()
}

/// Starts the one decode of `input` into `lanes` (1 or 2) lanes of peaks, handed over as they
/// fill: complete once the whole file was read and ffmpeg ended well, else with the error (a
/// read error, ffmpeg's exit and its last words, or a start that failed). `wake` is called on
/// each hand-over. Dropping what it returns stops it.
pub fn start(
    tools: &Tools,
    input: &Input,
    lanes: usize,
    duration: f64,
    wake: impl Fn() + Send + 'static,
) -> Decode {
    start_onto(tools, input, lanes, duration, None, Arc::default(), wake)
}

/// Tests: [`start`] with its child in `slot` (a start the test holds).
#[cfg(test)]
pub(crate) fn start_in(
    slot: Arc<Slot>,
    tools: &Tools,
    input: &Input,
    lanes: usize,
    duration: f64,
    wake: impl Fn() + Send + 'static,
) -> Decode {
    start_onto(tools, input, lanes, duration, None, slot, wake)
}

/// [`start`] for a file whose first `secs` seconds a preview has read (`before`: their peaks):
/// the decode goes on from there — nothing before it is decoded again — into the same peaks.
pub fn resume(
    tools: &Tools,
    input: &Input,
    lanes: usize,
    duration: f64,
    (before, secs): (Source, f64),
    wake: impl Fn() + Send + 'static,
) -> Decode {
    start_onto(
        tools,
        input,
        lanes,
        duration,
        Some((before, secs)),
        Arc::default(),
        wake,
    )
}

fn start_onto(
    tools: &Tools,
    input: &Input,
    lanes: usize,
    duration: f64,
    onto: Option<(Source, f64)>,
    child: Arc<Slot>,
    wake: impl Fn() + Send + 'static,
) -> Decode {
    let decode = Decode {
        // What was read before shows from the start.
        peaks: Arc::new(Mutex::new(onto.as_ref().map(|(before, _)| before.clone()))),
        child,
        #[cfg(test)]
        waiting: Arc::default(),
    };
    let (slot, running) = (decode.peaks.clone(), decode.child.clone());
    #[cfg(test)]
    let waiting = decode.waiting.clone();
    // A remote file's waveform is a Bulk reader's (06 §7.4).
    let (tools, input) = (tools.clone(), input.of_kind(Kind::Bulk));
    std::thread::spawn(move || {
        let from = onto.as_ref().map_or(0.0, |(_, secs)| *secs);
        let cuts = input.cuts();
        let mut cmd = command(&tools, &input, lanes, from, None);
        let failed = |why: String, last: Option<Source>| {
            let mut peaks = last.unwrap_or(Source {
                bin_secs: 1.0,
                lanes: Vec::new(),
                version: 0,
                complete: false,
                read: 0.0,
                error: None,
            });
            peaks.version += 1;
            peaks.complete = false;
            peaks.error = Some(why);
            peaks
        };
        #[cfg(test)]
        note_start(&input, from, None);
        let started = match running.start(&mut cmd, input.local()) {
            // Stopped before it started.
            Ok(None) => return,
            Ok(Some(pipes)) => Ok(pipes),
            Err(e) => Err(e),
        };
        let done = match started {
            Err(e) => failed(format!("cannot run {}: {e}", tools.ffmpeg.display()), None),
            Ok((mut stdout, stderr)) => {
                // From here on, every way out of this thread — unwinding too — kills and reaps
                // what is still in the slot: the owner's drop only signals.
                let _ending = Ending(&running);
                let said = crate::ffmpeg::ErrorTail::spawn(stderr);
                let onto = onto.as_ref().map(|(before, secs)| (before, *secs));
                // Read with the child watched from a thread of its own: a stop is applied
                // whatever this read blocks on, and the child's end is waited for with the
                // child still in its slot (an ffmpeg that closed its output and lives on is
                // there for the drop to kill).
                let watched = running.watched(|| {
                    let read = read_onto(&mut stdout, lanes, duration, onto, &mut |peaks| {
                        // Nothing is handed over once the decode was stopped.
                        if !running.stopped() {
                            *slot.lock().unwrap() = Some(peaks);
                            wake();
                        }
                    });
                    drop(stdout);
                    #[cfg(test)]
                    waiting.store(true, SeqCst);
                    read
                });
                if running.stopped() {
                    return;
                }
                let (read, status) = match watched {
                    Ok((read, status)) => (
                        read,
                        status.ok_or_else(|| std::io::Error::other("it is gone")),
                    ),
                    // Not watched, so not read: ended here, before its last words are waited
                    // for (they end only with it).
                    Err(e) => {
                        running.end();
                        (Err(e), Err(std::io::Error::other("not watched")))
                    }
                };
                let last_line = said.join().unwrap_or_default().last_line();
                // ffmpeg exits 0 on a stream cut short: the account says it (06 §7.4).
                let cut = input.cut_since(cuts);
                match (read, status) {
                    (Err(e), _) => failed(format!("reading the decoded sound: {e}"), None),
                    (Ok(peaks), _) if cut.is_some() => failed(cut.expect("checked"), Some(peaks)),
                    (Ok(peaks), Ok(s)) if s.success() => Source {
                        complete: true,
                        read: duration,
                        ..peaks
                    },
                    (Ok(peaks), Ok(s)) => {
                        let said = input.said(&last_line);
                        failed(format!("ffmpeg {s}: {said}"), Some(peaks))
                    }
                    (Ok(peaks), Err(e)) => failed(format!("ffmpeg: {e}"), Some(peaks)),
                }
            }
        };
        if running.stopped() {
            return;
        }
        let mut slot = slot.lock().unwrap();
        // A failed read keeps what was handed over before it.
        let done = match (slot.take(), done) {
            (Some(prev), d) if d.lanes.is_empty() => Source {
                version: prev.version + 1,
                complete: false,
                error: d.error,
                ..prev
            },
            (_, d) => d,
        };
        *slot = Some(done);
        drop(slot);
        wake();
    });
    decode
}

/// The timeline's lanes as `columns` columns: each column's min/max over the source ranges the
/// edit list puts under it. `lanes` (when the source has one lane, Mono → Stereo shows it twice).
pub fn columns(
    source: &Source,
    timeline: &Timeline,
    columns: usize,
    lanes: usize,
) -> Vec<Vec<[f32; 2]>> {
    let columns = columns.max(1);
    let len = timeline.len().max(1);
    let clips = timeline.clips();
    let bins = source.lanes.first().map_or(0, Vec::len);
    let bin_of = |source_tick: i64| -> usize {
        let secs = timeline.secs(source_tick);
        ((secs / source.bin_secs).floor().max(0.0) as usize).min(bins.saturating_sub(1))
    };
    if bins == 0 {
        return vec![vec![[0.0f32, 0.0f32]; columns]; lanes.max(1)];
    }
    let mut out: Vec<Vec<[f32; 2]>> = vec![Vec::with_capacity(columns); lanes.max(1)];
    // Walk the clips and the columns together: each column's ticks, clip by clip.
    let mut clip = 0usize;
    let mut clip_at = 0i64; // where `clips[clip]` starts on the timeline
    for x in 0..columns {
        let a = (x as i64 * len) / columns as i64;
        let b = (((x + 1) as i64 * len) / columns as i64).max(a + 1);
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        let mut t = a;
        while t < b && clip < clips.len() {
            let c: Clip = clips[clip];
            let end_on_timeline = clip_at + c.len();
            if t >= end_on_timeline {
                clip_at = end_on_timeline;
                clip += 1;
                continue;
            }
            let upto = b.min(end_on_timeline);
            let (s0, s1) = (c.start + (t - clip_at), c.start + (upto - clip_at));
            let (k0, k1) = (bin_of(s0), bin_of((s1 - 1).max(s0)));
            for (lane, (lo, hi)) in lo.iter_mut().zip(hi.iter_mut()).enumerate().take(2) {
                let src = &source.lanes[lane.min(source.lanes.len() - 1)];
                for bin in &src[k0..=k1] {
                    *lo = lo.min(bin[0]);
                    *hi = hi.max(bin[1]);
                }
            }
            t = upto;
        }
        for (lane, column) in out.iter_mut().enumerate() {
            let l = lane.min(1);
            column.push(if lo[l] <= hi[l] {
                [lo[l], hi[l]]
            } else {
                [0.0, 0.0]
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::media::timeline::View;

    #[test]
    fn peaks_bin_min_and_max() {
        let mut p = Peaks::new(10, 5);
        for v in [
            0.1, -0.2, 0.5, 0.3, -0.9, 0.0, 0.2, 0.2, 0.4, -0.1, 0.7, 0.8,
        ] {
            p.push(v);
        }
        assert_eq!(p.bins[0], [-0.2, 0.1]);
        assert_eq!(p.bins[2], [-0.9, 0.0]);
        // Samples past the estimate land in the last column.
        assert_eq!(p.bins[4], [-0.1, 0.8]);
    }

    /// Two channels interleaved come apart into two lanes; memory is the bins, whatever the
    /// length: an hour of stereo is at most `MAX_BINS` per lane.
    #[test]
    fn stereo_reads_into_two_lanes_and_the_bins_are_bounded() {
        let mut pcm = Vec::new();
        for k in 0..800 {
            let (l, r) = (if k < 400 { 0.5f32 } else { -0.5 }, 0.25f32);
            pcm.extend(l.to_le_bytes());
            pcm.extend(r.to_le_bytes());
        }
        let mut handed: Vec<Source> = Vec::new();
        let src = read_peaks(&mut pcm.as_slice(), 2, 0.1, &mut |s| handed.push(s)).unwrap();
        // Handed over as it fills (by sample count); none of it complete (that is `start`'s).
        assert!(handed.len() > 2, "{}", handed.len());
        assert!(handed.iter().all(|s| !s.complete && s.error.is_none()));
        let first = &handed[0];
        assert!(first.lanes[0].iter().any(|b| *b != [0.0, 0.0]));
        assert_eq!(first.lanes[0][9], [0.0, 0.0], "not reached yet");
        assert_eq!(src.version as usize, handed.len() + 1);
        assert_eq!(src.lanes.len(), 2);
        assert_eq!(src.lanes[0].len(), 10);
        // Each bin spans the zero line too (the waveform is drawn from it).
        assert_eq!(src.lanes[0][0], [0.0, 0.5]);
        assert_eq!(src.lanes[0][9], [-0.5, 0.0]);
        assert!(src.lanes[1].iter().all(|b| *b == [0.0, 0.25]));
        assert_eq!(bins_for(3600.0), 360_000);
        assert_eq!(bins_for(10.0 * 3600.0), MAX_BINS);
    }

    /// A read error is an error, not the end of the sound: `read_peaks` says so.
    #[test]
    fn a_read_error_is_not_the_end_of_the_sound() {
        struct Breaks(usize);
        impl Read for Breaks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::other("the pipe broke"));
                }
                let n = buf.len().min(self.0);
                buf[..n].fill(0);
                self.0 -= n;
                Ok(n)
            }
        }
        let got = read_peaks(&mut Breaks(4000), 1, 1.0, &mut |_| {});
        assert_eq!(got.unwrap_err().to_string(), "the pipe broke");
    }

    /// A decoder that fails part-way — some sound, then an error exit — leaves peaks that are
    /// not complete and an error saying what it said; one that cannot start says that.
    #[test]
    fn a_failing_decode_is_shown_as_a_failure_not_a_flat_waveform() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("ffmpeg");
        std::fs::write(
            &fake,
            "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\nhead -c 8000 /dev/zero\n\
             echo 'Invalid data found when processing input' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::testutil::run_fresh(std::process::Command::new(&fake).arg("-version"));
        let wait = |tools: Tools| -> Source {
            let (tx, rx) = std::sync::mpsc::channel();
            let shared = start(&tools, &Input::path(tmp.path()), 1, 1.0, move || {
                let _ = tx.send(());
            });
            loop {
                rx.recv_timeout(std::time::Duration::from_secs(20))
                    .expect("the decode ends");
                if let Some(s) = shared.peaks.lock().unwrap().clone() {
                    if s.complete || s.error.is_some() {
                        return s;
                    }
                }
            }
        };
        let got = wait(Tools {
            ffmpeg: fake.clone(),
            ffprobe: fake,
        });
        assert!(!got.complete);
        let error = got.error.unwrap();
        assert!(error.contains("Invalid data found"), "{error}");
        assert!(error.contains("exit"), "{error}");
        let gone = wait(Tools {
            ffmpeg: tmp.path().join("missing"),
            ffprobe: tmp.path().join("missing"),
        });
        assert!(gone.error.unwrap().contains("cannot run"));
    }

    /// A decoder that prints megabytes of errors — thousands of lines, then a 3 MB line, then
    /// its last words — is read to its end and its failure says that last line; what is kept is
    /// bounded ([`crate::ffmpeg::ErrorTail`]).
    #[test]
    fn a_noisy_decoder_s_failure_says_its_last_words_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("ffmpeg");
        std::fs::write(
            &fake,
            "#!/bin/sh\n[ \"$1\" = -version ] && exit 0\nhead -c 8000 /dev/zero\n\
             i=0; while [ $i -lt 2000 ]; do echo \"noise $i\" >&2; i=$((i+1)); done\n\
             head -c 3000000 /dev/zero | tr '\\0' x >&2\nprintf '\\nthe end' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::testutil::run_fresh(std::process::Command::new(&fake).arg("-version"));
        let tools = Tools {
            ffmpeg: fake.clone(),
            ffprobe: fake,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = start(&tools, &Input::path(tmp.path()), 1, 1.0, move || {
            let _ = tx.send(());
        });
        let got = loop {
            rx.recv_timeout(std::time::Duration::from_secs(20))
                .expect("the decode ends");
            if let Some(s) = shared.peaks.lock().unwrap().clone() {
                if s.complete || s.error.is_some() {
                    break s;
                }
            }
        };
        let error = got.error.unwrap();
        assert!(
            error.ends_with(": the end"),
            "{}",
            &error[..80.min(error.len())]
        );
        assert!(error.len() < 8192, "{} bytes", error.len());
    }

    /// A stream slower than the count of hand-overs: the peaks are handed over at least every
    /// [`HANDOVER_EVERY`] as they fill (06 §7.4), not only at each 1/32 of the file.
    #[test]
    fn a_slow_stream_is_handed_over_by_the_time() {
        struct Slow {
            chunks: usize,
            at: usize,
        }
        impl Read for Slow {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.chunks == 0 {
                    return Ok(0);
                }
                // 16 384 samples (mono f32) a chunk, a little over a hand-over apart.
                if self.at == 0 {
                    std::thread::sleep(HANDOVER_EVERY + std::time::Duration::from_millis(50));
                }
                let n = buf.len().min(16_384 * 4 - self.at);
                buf[..n].fill(0);
                self.at += n;
                if self.at == 16_384 * 4 {
                    (self.chunks, self.at) = (self.chunks - 1, 0);
                }
                Ok(n)
            }
        }
        let mut handed: Vec<f64> = Vec::new();
        // An hour of sound: a count step is 900 000 samples, far past what comes here.
        let done = read_peaks(&mut Slow { chunks: 3, at: 0 }, 1, 3600.0, &mut |s| {
            handed.push(s.read)
        })
        .unwrap();
        assert!(!handed.is_empty(), "handed over as it filled");
        assert!(
            handed[0] < done.read,
            "{handed:?} before the end at {}",
            done.read
        );
    }

    /// A slow stream that gives a few samples a read — 400, every 120 ms, far fewer than any
    /// count step or 4096 — is still handed over as it fills: at several distinct moments, each
    /// later than the one before, each reaching further, none of them complete.
    #[test]
    fn a_trickle_of_samples_is_handed_over_as_it_comes() {
        struct Trickle(usize);
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Ok(0);
                }
                self.0 -= 1;
                std::thread::sleep(std::time::Duration::from_millis(120));
                let n = buf.len().min(400 * 4);
                buf[..n].fill(0);
                Ok(n)
            }
        }
        let started = std::time::Instant::now();
        let mut handed: Vec<(std::time::Duration, f64, bool)> = Vec::new();
        let done = read_peaks(&mut Trickle(12), 1, 3600.0, &mut |s| {
            handed.push((started.elapsed(), s.read, s.complete))
        })
        .unwrap();
        assert!(handed.len() >= 3, "handed over as it came: {handed:?}");
        assert!(handed.iter().all(|h| !h.2 && h.1 > 0.0), "{handed:?}");
        assert!(
            handed
                .windows(2)
                .all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1),
            "later and further each time: {handed:?}"
        );
        assert!(
            handed.last().unwrap().1 <= done.read,
            "{handed:?} {}",
            done.read
        );
    }

    /// After a cut and a paste the columns show the source in the edit's order — from the same
    /// peaks, no decode.
    #[test]
    fn columns_follow_the_edit_list() {
        // 4 s of source, one bin per second: -1, -2, -3, -4 (min) / 1, 2, 3, 4 (max).
        let src = Source {
            bin_secs: 1.0,
            lanes: vec![(1..=4).map(|k| [-(k as f32), k as f32]).collect()],
            version: 1,
            complete: true,
            read: 4.0,
            error: None,
        };
        let mut t = Timeline::new(10.0, 40);
        let whole = columns(&src, &t, 4, 1);
        assert_eq!(
            whole[0],
            [[-1.0, 1.0], [-2.0, 2.0], [-3.0, 3.0], [-4.0, 4.0]]
        );
        // Move second 3 to the front.
        let piece = t.cut(20, 30, View::default()).unwrap();
        t.insert(0, &piece, View::default());
        let edited = columns(&src, &t, 4, 2);
        assert_eq!(
            edited[0],
            [[-3.0, 3.0], [-1.0, 1.0], [-2.0, 2.0], [-4.0, 4.0]]
        );
        assert_eq!(edited[1], edited[0], "one lane shown twice");
    }
}
