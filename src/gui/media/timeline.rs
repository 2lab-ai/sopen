//! The media window's edit list (spec §7, message 13): the output is a list of clips, each a
//! range of the source, played and rendered in order. Cut, copy, paste, delete, Mono → Stereo,
//! undo and redo change only this list — never a file, never ffmpeg; Save and Export render it.
//!
//! Times are whole ticks: the video's frames (so every cut point is a frame time and the audio
//! cut with it cannot drift), or the audio's samples. The list is kept normal: no empty clip, and
//! no two neighbours that continue each other in the source (they are one clip).

/// A position or a length in ticks (frames, or samples).
pub type Tick = i64;

/// A range of the source, `[start, end)` in source ticks, `start < end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clip {
    pub start: Tick,
    pub end: Tick,
}

impl Clip {
    pub fn len(&self) -> Tick {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// What an undo step restores: the clips and the channel mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub clips: Vec<Clip>,
    /// The output's left and right are both the (mono) source's one channel.
    pub stereo: bool,
}

/// Where the playhead and the selection were, in timeline ticks — restored with an undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct View {
    pub at: Tick,
    pub sel: Option<(Tick, Tick)>,
}

/// Undo steps kept (each is a short list of clips).
const HISTORY: usize = 512;

/// The total length of `clips`.
pub fn total(clips: &[Clip]) -> Tick {
    clips.iter().map(Clip::len).sum()
}

/// Drops empty clips and joins neighbours that continue each other in the source.
fn normalize(clips: &mut Vec<Clip>) {
    let mut out: Vec<Clip> = Vec::with_capacity(clips.len());
    for c in clips.drain(..).filter(|c| c.end > c.start) {
        match out.last_mut() {
            Some(last) if last.end == c.start => last.end = c.end,
            _ => out.push(c),
        }
    }
    *clips = out;
}

/// The clips covering timeline `[a, b)`.
pub fn extract(clips: &[Clip], a: Tick, b: Tick) -> Vec<Clip> {
    let mut out = Vec::new();
    let mut at = 0;
    for c in clips {
        let (lo, hi) = (a.max(at), b.min(at + c.len()));
        if hi > lo {
            out.push(Clip {
                start: c.start + (lo - at),
                end: c.start + (hi - at),
            });
        }
        at += c.len();
        if at >= b {
            break;
        }
    }
    out
}

/// The source tick shown at timeline tick `t` (the last clip's end at or past the end).
pub fn source_at(clips: &[Clip], t: Tick) -> Tick {
    let mut at = 0;
    for c in clips {
        if t < at + c.len() {
            return c.start + (t - at).max(0);
        }
        at += c.len();
    }
    clips.last().map_or(0, |c| c.end)
}

/// Where each clip starts on the timeline (the boundaries the window draws), the first at 0.
pub fn starts(clips: &[Clip]) -> Vec<Tick> {
    let mut at = 0;
    clips
        .iter()
        .map(|c| {
            let s = at;
            at += c.len();
            s
        })
        .collect()
}

/// The edit list of one window, with its undo history.
#[derive(Debug, Clone)]
pub struct Timeline {
    /// Ticks per second: the video's frame rate, or the audio's sample rate.
    pub rate: f64,
    /// The source's length in ticks.
    pub source: Tick,
    edit: Edit,
    undo: Vec<(Edit, View)>,
    redo: Vec<(Edit, View)>,
    /// Counts every change (the window caches what it draws per revision).
    rev: u64,
}

impl Timeline {
    /// The whole source, unedited.
    pub fn new(rate: f64, source: Tick) -> Self {
        let source = source.max(1);
        Self {
            rate,
            source,
            edit: Edit {
                clips: vec![Clip {
                    start: 0,
                    end: source,
                }],
                stereo: false,
            },
            undo: Vec::new(),
            redo: Vec::new(),
            rev: 0,
        }
    }

    pub fn clips(&self) -> &[Clip] {
        &self.edit.clips
    }

    pub fn stereo(&self) -> bool {
        self.edit.stereo
    }

    pub fn len(&self) -> Tick {
        total(&self.edit.clips)
    }

    /// Never: an edit cannot remove all of it.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Seconds → the nearest tick.
    pub fn tick(&self, secs: f64) -> Tick {
        (secs * self.rate).round() as Tick
    }

    /// Ticks → seconds.
    pub fn secs(&self, ticks: Tick) -> f64 {
        ticks as f64 / self.rate
    }

    /// The output is the source as it is: nothing to save.
    pub fn is_identity(&self) -> bool {
        !self.edit.stereo
            && self.edit.clips
                == [Clip {
                    start: 0,
                    end: self.source,
                }]
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// `[a, b)` clamped to the timeline; `None` when it is empty.
    fn range(&self, a: Tick, b: Tick) -> Option<(Tick, Tick)> {
        let (a, b) = (a.min(b).clamp(0, self.len()), a.max(b).clamp(0, self.len()));
        (b > a).then_some((a, b))
    }

    /// Applies `next` as one undo step taken from `view`; false when it changes nothing.
    fn commit(&mut self, mut next: Edit, view: View) -> bool {
        normalize(&mut next.clips);
        if next == self.edit {
            return false;
        }
        self.undo
            .push((std::mem::replace(&mut self.edit, next), view));
        if self.undo.len() > HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.rev += 1;
        true
    }

    /// The clips of timeline `[a, b)` (what Copy puts on the clipboard).
    pub fn copy(&self, a: Tick, b: Tick) -> Option<Vec<Clip>> {
        let (a, b) = self.range(a, b)?;
        Some(extract(&self.edit.clips, a, b))
    }

    /// Removes timeline `[a, b)` and closes the gap; returns what was removed.
    pub fn cut(&mut self, a: Tick, b: Tick, view: View) -> Option<Vec<Clip>> {
        let (a, b) = self.range(a, b)?;
        // An output must keep something: removing all of it is refused.
        if a == 0 && b == self.len() {
            return None;
        }
        let removed = extract(&self.edit.clips, a, b);
        let mut kept = extract(&self.edit.clips, 0, a);
        kept.extend(extract(&self.edit.clips, b, self.len()));
        self.commit(
            Edit {
                clips: kept,
                stereo: self.edit.stereo,
            },
            view,
        )
        .then_some(removed)
    }

    /// Whether every clip of `piece` is a range of this source (not empty, inside it).
    fn fits(&self, piece: &[Clip]) -> bool {
        !piece.is_empty()
            && piece
                .iter()
                .all(|c| c.start >= 0 && c.end > c.start && c.end <= self.source)
    }

    /// Puts `piece` at timeline tick `at` (pushing what follows later); refused (false) unless
    /// every clip of it is a range of this source.
    pub fn insert(&mut self, at: Tick, piece: &[Clip], view: View) -> bool {
        if !self.fits(piece) {
            return false;
        }
        let piece = piece.to_vec();
        let at = at.clamp(0, self.len());
        let mut clips = extract(&self.edit.clips, 0, at);
        clips.extend(piece);
        clips.extend(extract(&self.edit.clips, at, self.len()));
        self.commit(
            Edit {
                clips,
                stereo: self.edit.stereo,
            },
            view,
        )
    }

    /// Replaces timeline `[a, b)` with `piece`, as one undo step; refused (false) unless every
    /// clip of it is a range of this source.
    pub fn replace(&mut self, a: Tick, b: Tick, piece: &[Clip], view: View) -> bool {
        let Some((a, b)) = self.range(a, b) else {
            return self.insert(a, piece, view);
        };
        if !self.fits(piece) {
            return false;
        }
        let piece = piece.to_vec();
        let mut clips = extract(&self.edit.clips, 0, a);
        clips.extend(piece);
        clips.extend(extract(&self.edit.clips, b, self.len()));
        self.commit(
            Edit {
                clips,
                stereo: self.edit.stereo,
            },
            view,
        )
    }

    /// Sets the Mono → Stereo mapping, as one undo step.
    pub fn set_stereo(&mut self, on: bool, view: View) -> bool {
        self.commit(
            Edit {
                clips: self.edit.clips.clone(),
                stereo: on,
            },
            view,
        )
    }

    /// Back one step; returns where the playhead and selection were before it.
    pub fn undo(&mut self, now: View) -> Option<View> {
        let (edit, view) = self.undo.pop()?;
        self.redo
            .push((std::mem::replace(&mut self.edit, edit), now));
        self.rev += 1;
        Some(view)
    }

    /// Forward one step; returns where the playhead and selection were when it was undone.
    pub fn redo(&mut self, now: View) -> Option<View> {
        let (edit, view) = self.redo.pop()?;
        self.undo
            .push((std::mem::replace(&mut self.edit, edit), now));
        self.rev += 1;
        Some(view)
    }

    /// Source ranges from timeline tick `t` to the end, in play order (what playback decodes).
    pub fn from(&self, t: Tick) -> Vec<Clip> {
        extract(&self.edit.clips, t.clamp(0, self.len()), self.len())
    }

    /// Replaces the list wholesale (the receipt hook's `clips=`), as one undo step.
    pub fn set_clips(&mut self, clips: Vec<Clip>, view: View) -> bool {
        let clips: Vec<Clip> = clips
            .into_iter()
            .map(|c| Clip {
                start: c.start.clamp(0, self.source),
                end: c.end.clamp(0, self.source),
            })
            .filter(|c| c.end > c.start)
            .collect();
        if clips.is_empty() {
            return false;
        }
        self.commit(
            Edit {
                clips,
                stereo: self.edit.stereo,
            },
            view,
        )
    }
}

#[cfg(test)]
mod tests;
