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
    /// The decode has read the whole file.
    pub complete: bool,
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
/// time) and at the end (`complete`).
pub fn read_peaks(
    input: &mut dyn Read,
    lanes: usize,
    duration: f64,
    publish: &mut dyn FnMut(Source),
) {
    let lanes = lanes.max(1);
    let bins = bins_for(duration);
    let expected = (duration * RATE).ceil() as usize;
    let mut peaks: Vec<Peaks> = (0..lanes).map(|_| Peaks::new(expected, bins)).collect();
    let mut reader = BufReader::with_capacity(64 * 1024, input);
    let step = (expected * lanes / HANDOVERS).max(1);
    let per_bin = peaks[0].per_bin;
    let mut version = 0;
    let mut snapshot = |peaks: &[Peaks], complete: bool| {
        version += 1;
        Source {
            bin_secs: per_bin as f64 / RATE,
            lanes: peaks.iter().map(|p| p.bins.clone()).collect(),
            version,
            complete,
        }
    };
    let mut b4 = [0u8; 4];
    let (mut c, mut n) = (0, 0usize);
    while reader.read_exact(&mut b4).is_ok() {
        peaks[c].push(f32::from_le_bytes(b4));
        c = (c + 1) % lanes;
        n += 1;
        if n % step == 0 {
            publish(snapshot(&peaks, false));
        }
    }
    publish(snapshot(&peaks, true));
}

/// Starts the one decode of `input` into `lanes` (1 or 2) lanes of peaks, handed to `out` as
/// they fill; `wake` is called on each hand-over (and when a failed decode ends).
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
            .stderr(Stdio::null());
        if let Ok(mut child) = crate::ffmpeg::spawn(&mut cmd) {
            let mut stdout = child.stdout.take().expect("piped");
            read_peaks(&mut stdout, lanes, duration, &mut |peaks| {
                *slot.lock().unwrap() = Some(peaks);
                wake();
            });
            let _ = child.wait();
        }
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
    let mut out = vec![vec![[0.0f32, 0.0f32]; columns]; lanes.max(1)];
    if bins == 0 {
        return out;
    }
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
            if lo[l] <= hi[l] {
                column[x] = [lo[l], hi[l]];
            }
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
        read_peaks(&mut pcm.as_slice(), 2, 0.1, &mut |s| handed.push(s));
        // Handed over as it fills (by sample count), complete once at the end.
        assert!(handed.len() > 2, "{}", handed.len());
        assert!(handed[..handed.len() - 1].iter().all(|s| !s.complete));
        let first = &handed[0];
        assert!(first.lanes[0].iter().any(|b| *b != [0.0, 0.0]));
        assert_eq!(first.lanes[0][9], [0.0, 0.0], "not reached yet");
        let src = handed.pop().unwrap();
        assert!(src.complete);
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
