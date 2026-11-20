use super::*;

/// The timeline as the reference sees it: the source tick shown at each timeline tick.
fn expand(clips: &[Clip]) -> Vec<Tick> {
    clips.iter().flat_map(|c| c.start..c.end).collect()
}

fn clip(start: Tick, end: Tick) -> Clip {
    Clip { start, end }
}

/// A small deterministic generator (no crate, no wall clock): the same cases every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: i64) -> i64 {
        (self.next() % n.max(1) as u64) as i64
    }
}

fn normal(clips: &[Clip]) -> bool {
    clips.iter().all(|c| c.end > c.start) && clips.windows(2).all(|w| w[0].end != w[1].start)
}

#[test]
fn cut_closes_the_gap_and_returns_what_it_removed() {
    let v = View::default();
    let mut t = Timeline::new(10.0, 100);
    assert!(t.is_identity());
    let removed = t.cut(20, 30, v).unwrap();
    assert_eq!(removed, [clip(20, 30)]);
    assert_eq!(t.clips(), [clip(0, 20), clip(30, 100)]);
    assert_eq!(t.len(), 90);
    assert!(!t.is_identity());
    // A range given backwards is the same range; an empty one is nothing.
    assert_eq!(t.copy(10, 5).unwrap(), [clip(5, 10)]);
    assert!(t.copy(7, 7).is_none());
    assert!(t.cut(7, 7, v).is_none());
    // The whole output cannot be cut away.
    assert!(t.cut(0, t.len(), v).is_none());
}

#[test]
fn paste_inserts_at_the_playhead_or_replaces_the_selection() {
    let v = View::default();
    let mut t = Timeline::new(10.0, 100);
    let piece = t.copy(10, 20).unwrap();
    // Insert at 50: everything after moves later by the piece's length.
    assert!(t.insert(50, &piece, v));
    assert_eq!(t.clips(), [clip(0, 50), clip(10, 20), clip(50, 100)]);
    assert_eq!(t.len(), 110);
    // Replace [0, 30) with the same piece: the output shrinks by 20.
    assert!(t.replace(0, 30, &piece, v));
    assert_eq!(
        t.clips(),
        [clip(10, 20), clip(30, 50), clip(10, 20), clip(50, 100)]
    );
    assert_eq!(t.len(), 90);
    // An empty selection is an insert at its place.
    assert!(t.replace(5, 5, &[clip(0, 1)], v));
    assert_eq!(t.len(), 91);
    assert_eq!(source_at(t.clips(), 5), 0);
}

#[test]
fn neighbours_that_continue_each_other_become_one_clip() {
    let v = View::default();
    let mut t = Timeline::new(25.0, 100);
    let removed = t.cut(40, 60, v).unwrap();
    // Putting it back where it was is the original again — not unsaved.
    assert!(t.insert(40, &removed, v));
    assert_eq!(t.clips(), [clip(0, 100)]);
    assert!(t.is_identity());
}

#[test]
fn mono_to_stereo_is_an_edit_with_its_own_undo_step() {
    let mut t = Timeline::new(48_000.0, 48_000);
    t.set_stereo(true, View::default());
    assert!(t.stereo() && !t.is_identity());
    assert!(t.undo(View::default()).is_some());
    assert!(!t.stereo() && t.is_identity());
    assert!(t.redo(View::default()).is_some());
    assert!(t.stereo());
}

#[test]
fn undo_and_redo_restore_the_list_and_the_view_it_was_made_from() {
    let mut t = Timeline::new(10.0, 100);
    let before = View {
        at: 33,
        sel: Some((20, 30)),
    };
    t.cut(20, 30, before).unwrap();
    let after = View { at: 20, sel: None };
    assert_eq!(t.undo(after), Some(before));
    assert!(t.is_identity());
    assert_eq!(t.redo(before), Some(after));
    assert_eq!(t.len(), 90);
    // A new edit drops what could be redone.
    t.undo(after);
    t.insert(0, &[clip(0, 5)], before);
    assert!(!t.can_redo());
}

/// Any sequence of cuts, copies, pastes (insert and replace), Mono → Stereo, undos and redos:
/// the list always equals the reference (a plain vector of the source tick at each output
/// tick, edited by splicing), its length is the sum of its clips, it stays normal and inside
/// the source, timeline → source agrees with the reference at every tick, and undoing every
/// step gives the untouched source back — redoing them all, the same output again.
#[test]
fn any_sequence_of_edits_matches_the_reference_and_undoes_to_the_source() {
    for seed in 1..=200u64 {
        let mut rng = Lcg(seed);
        let source = 20 + rng.below(60);
        let mut t = Timeline::new(10.0, source);
        let mut reference: Vec<Tick> = (0..source).collect();
        let mut clipboard: Vec<Clip> = Vec::new();
        let mut board_ref: Vec<Tick> = Vec::new();
        let mut steps = 0usize;
        for op in 0..40 {
            let len = t.len();
            let (a, b) = (rng.below(len + 1), rng.below(len + 1));
            let (lo, hi) = (a.min(b) as usize, a.max(b) as usize);
            let view = View {
                at: a,
                sel: Some((a, b)),
            };
            let changed = match rng.below(6) {
                0 => {
                    if let Some(piece) = t.copy(a, b) {
                        board_ref = reference[lo..hi].to_vec();
                        clipboard = piece;
                    }
                    false
                }
                1 => match t.cut(a, b, view) {
                    Some(piece) => {
                        board_ref = reference.drain(lo..hi).collect();
                        clipboard = piece;
                        true
                    }
                    None => false,
                },
                2 if !clipboard.is_empty() => {
                    let at = a as usize;
                    let ok = t.insert(a, &clipboard, view);
                    reference.splice(at..at, board_ref.iter().copied());
                    ok
                }
                3 if !clipboard.is_empty() => {
                    let ok = t.replace(a, b, &clipboard, view);
                    reference.splice(lo..hi, board_ref.iter().copied());
                    ok
                }
                4 => {
                    let before = t.stereo();
                    t.set_stereo(!before, view)
                }
                _ => false,
            };
            steps += usize::from(changed);
            let case = format!("seed {seed} op {op}");
            assert_eq!(expand(t.clips()), reference, "{case}");
            assert_eq!(t.len(), total(t.clips()), "{case}");
            assert_eq!(t.len() as usize, reference.len(), "{case}");
            assert!(normal(t.clips()), "{case}: {:?}", t.clips());
            assert!(
                t.clips().iter().all(|c| c.start >= 0 && c.end <= source),
                "{case}"
            );
            for (k, want) in reference.iter().enumerate() {
                assert_eq!(source_at(t.clips(), k as Tick), *want, "{case} tick {k}");
            }
            // What playback decodes from any tick is the rest of the reference.
            let from = rng.below(t.len() + 1);
            assert_eq!(expand(&t.from(from)), reference[from as usize..], "{case}");
        }
        let edited = t.clips().to_vec();
        let stereo = t.stereo();
        let mut undone = 0;
        while t.undo(View::default()).is_some() {
            undone += 1;
        }
        assert_eq!(undone, steps.min(HISTORY), "seed {seed}");
        assert!(t.is_identity(), "seed {seed}: {:?}", t.clips());
        while t.redo(View::default()).is_some() {}
        assert_eq!((t.clips(), t.stereo()), (edited.as_slice(), stereo));
    }
}

#[test]
fn ticks_and_seconds() {
    let t = Timeline::new(25.0, 100);
    assert_eq!(t.tick(1.0), 25);
    assert_eq!(t.tick(1.019), 25);
    assert_eq!(t.tick(1.021), 26);
    assert_eq!(t.secs(50), 2.0);
    assert_eq!(starts(&[clip(5, 10), clip(0, 3), clip(50, 51)]), [0, 5, 8]);
}
