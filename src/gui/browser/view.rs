//! What each side of the browser shows (02 §2–§3): the mode, and [`resolve`], a pure function of
//! 02 §3.1's inputs (the committed display among them) that the browser asks on every pass. S1-E
//! wrote the rules O2, O5 and O7, the 150 ms commit and their table test; S5-C added the tree's
//! O4 (rule 3's Tree and its `tree_snapshot`); S2-F adds O6 and the pin.

use std::time::Duration;

/// How long the bar rests on an entry before the other side follows it (02 §3.1 "Committing",
/// §4.1): a sweep over files and folders prepares nothing and changes nothing on the way.
pub const COMMIT: Duration = Duration::from_millis(150);

/// The browser's mode (02 §2). It belongs to the window and is not saved; F9 switches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The side opposite the active list follows its bar (the default).
    #[default]
    Preview,
    /// Two lists, as the browser shipped with message 12.
    Dual,
}

impl Mode {
    /// The mode F9 switches to.
    pub fn other(self) -> Mode {
        match self {
            Mode::Preview => Mode::Dual,
            Mode::Dual => Mode::Preview,
        }
    }
}

/// What one side shows (02 §1), with its identity. S1 and S5 have these; the git panel (S2-F)
/// adds its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// The side's own panel.
    List,
    /// The preview of this file — the path under the active list's bar (02 §4).
    Preview(String),
    /// The directory tree, in place of the side's list (05 §1): the side with the keyboard.
    Tree,
    /// A viewer opened by Enter or F3 covers the side (rule 1).
    Viewer,
}

/// The active list's bar (02 §3.1 `bar`): what O5 asks of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Bar {
    /// A regular file or a link to one, by its path.
    File(String),
    /// A folder, a link to a folder, `..`, a special entry, or an empty list.
    #[default]
    Other,
}

/// 02 §3.1's inputs, those S1 and S5 have (`origin`, `folder`'s detection, `pin` and `git_wide`
/// come with S2).
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub mode: Mode,
    /// The side that has the keyboard (0 left, 1 right): in its List, or in its Tree while
    /// `tree_snapshot` is there.
    pub focus: usize,
    /// The tree is open on the focus side (05 §1): what the other side showed when it opened
    /// (O4), with its identity — shown, unchanged, until the tree closes.
    pub tree_snapshot: Option<Content>,
    pub bar: Bar,
    /// How long the bar has rested since it last moved within the same folder; `None` when the
    /// last change was anything else — the window opening, a focus, mode or folder change, a
    /// viewer closing — which commits at once.
    pub rest: Option<Duration>,
    /// The committed display: what each side shows now. `None` before the first pass (the
    /// first target is the first display).
    pub shown: Option<[Content; 2]>,
    /// A viewer covers both sides.
    pub viewer: bool,
}

/// What `resolve` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// What the two sides show now, `[left, right]`: the new committed display.
    pub show: [Content; 2],
    /// A side keeps what it showed although the rules name something else: the bar has not
    /// rested yet, and this is how long until it has (the browser asks again then).
    pub pending: Option<Duration>,
}

/// What the rules name for the two sides, whatever they show now (02 §3.1 rules 1, 3 and 4).
pub fn target(inputs: &Inputs) -> [Content; 2] {
    if inputs.viewer {
        return [Content::Viewer, Content::Viewer];
    }
    let other = match (inputs.mode, &inputs.tree_snapshot, &inputs.bar) {
        // O2: dual mode — its own list.
        (Mode::Dual, _, _) => Content::List,
        // O4: the keyboard is in the tree — what the side showed when it opened.
        (Mode::Preview, Some(snapshot), _) => snapshot.clone(),
        // O5: the bar on a file — its preview.
        (Mode::Preview, None, Bar::File(path)) => Content::Preview(path.clone()),
        // O7 (O6, the git panel, comes with S2-F): its own list.
        (Mode::Preview, None, Bar::Other) => Content::List,
    };
    // Rule 3: the side with the keyboard shows what the keyboard is in — its List, or its Tree.
    let own = match inputs.tree_snapshot {
        Some(_) => Content::Tree,
        None => Content::List,
    };
    if inputs.focus == 0 {
        [own, other]
    } else {
        [other, own]
    }
}

/// What the two sides show (02 §3.1): the rules' target, except that a side whose target
/// differs from what it shows only because the bar moved keeps what it shows until the bar has
/// rested [`COMMIT`].
pub fn resolve(inputs: &Inputs) -> Resolved {
    let target = target(inputs);
    let (Some(shown), Some(rest)) = (&inputs.shown, inputs.rest) else {
        return Resolved {
            show: target,
            pending: None,
        };
    };
    if rest >= COMMIT || *shown == target {
        return Resolved {
            show: target,
            pending: None,
        };
    }
    Resolved {
        show: shown.clone(),
        pending: Some(COMMIT - rest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> Bar {
        Bar::File(path.to_string())
    }

    fn preview(path: &str) -> Content {
        Content::Preview(path.to_string())
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// 02 §3.1's table, every row S1 has (rule 1, rule 3, O2, O5, O7) with every input of the
    /// state table S1 has: both modes, either focus, the bar on a file and on anything else, a
    /// viewer open or not.
    #[test]
    fn the_rules_name_each_sides_content() {
        use Content::{List, Viewer};
        for mode in [Mode::Preview, Mode::Dual] {
            for focus in [0, 1] {
                for bar in [file("/d/a.rs"), Bar::Other] {
                    for viewer in [false, true] {
                        let inputs = Inputs {
                            mode,
                            focus,
                            bar: bar.clone(),
                            viewer,
                            ..Default::default()
                        };
                        let got = target(&inputs);
                        let said = format!("{mode:?} focus {focus} {bar:?} viewer {viewer}");
                        if viewer {
                            // Rule 1.
                            assert_eq!(got, [Viewer, Viewer], "{said}");
                            continue;
                        }
                        // Rule 3: the side with the keyboard is its list.
                        assert_eq!(got[focus], List, "{said}");
                        let other = &got[1 - focus];
                        match (mode, &bar) {
                            (Mode::Dual, _) => assert_eq!(*other, List, "O2: {said}"),
                            (Mode::Preview, Bar::File(_)) => {
                                assert_eq!(*other, preview("/d/a.rs"), "O5: {said}")
                            }
                            (Mode::Preview, Bar::Other) => assert_eq!(*other, List, "O7: {said}"),
                        }
                        // With nothing shown yet (the window opening) the target is the display.
                        assert_eq!(resolve(&inputs).show, got, "{said}");
                        assert_eq!(resolve(&inputs).pending, None, "{said}");
                    }
                }
            }
        }
    }

    /// "Committing": a side whose target changed because the bar moved keeps what it shows
    /// until 150 ms have passed, and the answer says how long that still is.
    #[test]
    fn a_bar_move_commits_after_150_ms() {
        let shown = [Content::List, preview("/d/w.txt")];
        let at = |rest: u64| Inputs {
            bar: file("/d/x.txt"),
            rest: Some(MS(rest)),
            shown: Some(shown.clone()),
            ..Default::default()
        };
        for rest in [0, 1, 149] {
            let got = resolve(&at(rest));
            assert_eq!(got.show, shown, "{rest} ms: the side keeps W's preview");
            assert_eq!(got.pending, Some(MS(150 - rest)), "{rest} ms");
        }
        for rest in [150, 151, 10_000] {
            let got = resolve(&at(rest));
            assert_eq!(got.show, [Content::List, preview("/d/x.txt")], "{rest} ms");
            assert_eq!(got.pending, None, "{rest} ms");
        }
        // A bar that came back to what is shown has nothing to wait for.
        let back = Inputs {
            bar: file("/d/w.txt"),
            rest: Some(MS(10)),
            shown: Some(shown.clone()),
            ..Default::default()
        };
        assert_eq!(resolve(&back).pending, None);
        assert_eq!(resolve(&back).show, shown);
    }

    /// P13: two histories reach the bar on X within 150 ms — one from a preview of W, one from
    /// the list (the bar was on a folder) — and each side keeps its own display until the
    /// deadline, then both show X's preview.
    #[test]
    fn p13_two_histories_keep_their_displays_until_the_deadline() {
        let from_preview = [Content::List, preview("/d/w.txt")];
        let from_list = [Content::List, Content::List];
        let on_x = |shown: &[Content; 2], rest: u64| Inputs {
            bar: file("/d/x.txt"),
            rest: Some(MS(rest)),
            shown: Some(shown.clone()),
            ..Default::default()
        };
        assert_eq!(resolve(&on_x(&from_preview, 100)).show, from_preview);
        assert_eq!(resolve(&on_x(&from_list, 100)).show, from_list);
        let x = [Content::List, preview("/d/x.txt")];
        assert_eq!(resolve(&on_x(&from_preview, 150)).show, x);
        assert_eq!(resolve(&on_x(&from_list, 150)).show, x);
    }

    /// Rule 3 and O4 (02 §3.1): while the tree is open the side with the keyboard shows it, and
    /// the other side shows the snapshot taken when it opened — whatever the bar's rest, the
    /// mode's O2 aside — until it closes; then the rules apply again.
    #[test]
    fn o4_the_other_side_is_the_snapshot_while_the_tree_is_open() {
        use Content::{List, Tree};
        for focus in [0, 1] {
            for snapshot in [List, preview("/d/w.txt")] {
                let inputs = Inputs {
                    focus,
                    // The bar of the list under the tree is on another file now (a re-read).
                    bar: file("/d/x.txt"),
                    tree_snapshot: Some(snapshot.clone()),
                    rest: Some(MS(10_000)),
                    shown: Some([List, List]),
                    ..Default::default()
                };
                let got = resolve(&inputs);
                assert_eq!(got.show[focus], Tree, "focus {focus}");
                assert_eq!(got.show[1 - focus], snapshot, "O4, focus {focus}");
                assert_eq!(got.pending, None);
                // Dual mode: O2 comes first, the other side's own list.
                let dual = Inputs {
                    mode: Mode::Dual,
                    ..inputs.clone()
                };
                assert_eq!(resolve(&dual).show[1 - focus], List, "O2");
                // The tree closed: O5 again.
                let closed = Inputs {
                    tree_snapshot: None,
                    rest: None,
                    ..inputs
                };
                assert_eq!(resolve(&closed).show[1 - focus], preview("/d/x.txt"));
                assert_eq!(resolve(&closed).show[focus], List);
            }
        }
    }

    /// Every change but a bar move commits at once (`rest` is `None` then): Tab, F9, entering
    /// a folder, a viewer opening or closing — also in the middle of the 150 ms.
    #[test]
    fn every_other_change_commits_at_once() {
        let shown = [Content::List, preview("/d/w.txt")];
        let base = Inputs {
            bar: file("/d/x.txt"),
            rest: None,
            shown: Some(shown.clone()),
            ..Default::default()
        };
        // Tab: the keyboard on the right; its bar is on `..`.
        let tab = Inputs {
            focus: 1,
            bar: Bar::Other,
            ..base.clone()
        };
        assert_eq!(resolve(&tab).show, [Content::List, Content::List]);
        // F9.
        let f9 = Inputs {
            mode: Mode::Dual,
            ..base.clone()
        };
        assert_eq!(resolve(&f9).show, [Content::List, Content::List]);
        // A folder entered: the bar is on `..`.
        let entered = Inputs {
            bar: Bar::Other,
            ..base.clone()
        };
        assert_eq!(resolve(&entered).show, [Content::List, Content::List]);
        // A viewer, and its closing.
        let viewer = Inputs {
            viewer: true,
            ..base.clone()
        };
        assert_eq!(resolve(&viewer).show, [Content::Viewer, Content::Viewer]);
        let closed = Inputs {
            shown: Some([Content::Viewer, Content::Viewer]),
            ..base.clone()
        };
        assert_eq!(
            resolve(&closed).show,
            [Content::List, preview("/d/x.txt")],
            "a viewer closing commits at once"
        );
        for inputs in [tab, f9, entered, viewer, closed] {
            assert_eq!(resolve(&inputs).pending, None);
        }
        assert_eq!(Mode::Preview.other(), Mode::Dual);
        assert_eq!(Mode::Dual.other(), Mode::Preview);
    }
}
