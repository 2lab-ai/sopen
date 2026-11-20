//! The audio window's waveform (spec §7): one decode of the whole file into min/max peaks per
//! channel — two lanes for stereo (more channels are mixed down to two), one for mono — at a
//! fixed resolution of the source, never the PCM itself. The peaks are handed over as they
//! fill (a streamed file arrives over time), and the timeline on screen is drawn from them
//! through the edit list, so an edit redraws it at once, with no decode.

use super::timeline::{Clip, Timeline};
use crate::ffmpeg::{Input, Tools};
use std::io::{BufReader, Read};
use std::process::{Command, Stdio};
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
    /// The decode failed (what it said): the peaks are what it read before, never complete.
    pub error: Option<String>,
}

/// Filled in by the decoder thread as it reads, then once more when it is done.
pub type Shared = Arc<Mutex<Option<Source>>>;

/// Hand-overs while decoding: the peaks so far, this many times over the file.
const HANDOVERS: usize = 32;

/// How many bins a `duration`-second source gets.
pub fn bins_for(duration: f64) -> usize {
    ((duration * BINS_PER_S).ceil() as usize).clamp(1, MAX_BINS)
}

/// Reads `input` (interleaved f32le, `lanes` channels at [`RATE`]) into peaks, handing the
/// peaks so far to `publish` every 1/[`HANDOVERS`] of the expected samples (by count, not by
/// time). Returns the peaks at the end of its input, or the read error that stopped it (the
/// end of input is not an error; a sample cut short at it is dropped). Nothing it publishes
/// is complete: whether the decode ended well is the caller's to say ([`start`]).
pub fn read_peaks(
    input: &mut dyn Read,
    lanes: usize,
    duration: f64,
    publish: &mut dyn FnMut(Source),
) -> std::io::Result<Source> {
    let lanes = lanes.max(1);
    let bins = bins_for(duration);
    let expected = (duration * RATE).ceil() as usize;
    let mut peaks: Vec<Peaks> = (0..lanes).map(|_| Peaks::new(expected, bins)).collect();
    let mut reader = BufReader::with_capacity(64 * 1024, input);
    let step = (expected * lanes / HANDOVERS).max(1);
    let per_bin = peaks[0].per_bin;
    let mut version = 0;
    let mut snapshot = |peaks: &[Peaks]| {
        version += 1;
        Source {
            bin_secs: per_bin as f64 / RATE,
            lanes: peaks.iter().map(|p| p.bins.clone()).collect(),
            version,
            complete: false,
            error: None,
        }
    };
    let mut b4 = [0u8; 4];
    let (mut c, mut n) = (0, 0usize);
    loop {
        match reader.read_exact(&mut b4) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        peaks[c].push(f32::from_le_bytes(b4));
        c = (c + 1) % lanes;
        n += 1;
        if n % step == 0 {
            publish(snapshot(&peaks));
        }
    }
    Ok(snapshot(&peaks))
}

/// Starts the one decode of `input` into `lanes` (1 or 2) lanes of peaks, handed to `out` as
/// they fill: complete once the whole file was read and ffmpeg ended well, else with the error
/// (a read error, ffmpeg's exit and its last words, or a start that failed). `wake` is called on
/// each hand-over.
pub fn start(
    tools: &Tools,
    input: &Input,
    lanes: usize,
    duration: f64,
    wake: impl Fn() + Send + 'static,
) -> Shared {
    let out: Shared = Arc::default();
    let (slot, tools, input) = (out.clone(), tools.clone(), input.clone());
    std::thread::spawn(move || {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(input.arg())
            .args(["-vn", "-ac", &lanes.to_string(), "-ar"])
            .arg(format!("{RATE}"))
            .args(["-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let failed = |why: String, last: Option<Source>| {
            let mut peaks = last.unwrap_or(Source {
                bin_secs: 1.0,
                lanes: Vec::new(),
                version: 0,
                complete: false,
                error: None,
            });
            peaks.version += 1;
            peaks.complete = false;
            peaks.error = Some(why);
            peaks
        };
        let done = match crate::ffmpeg::spawn(&mut cmd) {
            Err(e) => failed(format!("cannot run {}: {e}", tools.ffmpeg.display()), None),
            Ok(mut child) => {
                let mut stdout = child.stdout.take().expect("piped");
                let mut stderr = child.stderr.take().expect("piped");
                let said = std::thread::spawn(move || {
                    let mut text = String::new();
                    let _ = stderr.read_to_string(&mut text);
                    text
                });
                let read = read_peaks(&mut stdout, lanes, duration, &mut |peaks| {
                    *slot.lock().unwrap() = Some(peaks);
                    wake();
                });
                drop(stdout);
                let status = child.wait();
                let said = said.join().unwrap_or_default();
                let last_line = said
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("no error output")
                    .trim()
                    .to_string();
                match (read, status) {
                    (Err(e), _) => failed(format!("reading the decoded sound: {e}"), None),
                    (Ok(peaks), Ok(s)) if s.success() => Source {
                        complete: true,
                        ..peaks
                    },
                    (Ok(peaks), Ok(s)) => failed(format!("ffmpeg {s}: {last_line}"), Some(peaks)),
                    (Ok(peaks), Err(e)) => failed(format!("ffmpeg: {e}"), Some(peaks)),
                }
            }
        };
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
    out
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
                if let Some(s) = shared.lock().unwrap().clone() {
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
