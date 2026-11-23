//! What each side of the browser shows (02 §2–§3): the mode, and [`resolve`], a pure function of
//! 02 §3.1's inputs (the committed display among them) that the browser asks on every pass. S1-E
//! wrote the rules O2, O5 and O7, the 150 ms commit and their table test; S5-C added the tree's
//! O4 (rule 3's Tree and its `tree_snapshot`); S2-F added the git panel: O3, O6, the pin (rule
//! 2) and `git_wide`.

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

/// What one side shows (02 §1), with its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// The side's own panel.
    List,
    /// The preview of this file — the path under the active list's bar (02 §4).
    Preview(String),
    /// The directory tree, in place of the side's list (05 §1): the side with the keyboard.
    Tree,
    /// The git panel of the repository whose top level this is (03; O6, the pin, the keyboard
    /// in it).
    Git(String),
    /// A viewer opened by Enter or F3 covers the side (rule 1).
    Viewer,
}

/// The pin (02 §3.1 `pin`, Cmd/Ctrl+G): the side the git panel is pinned to and the
/// repository's top level. It belongs to the side, not to the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub side: usize,
    pub repo: String,
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

/// 02 §3.1's inputs. `origin` (the list the keyboard left for a Git) is the browser's: the
/// keyboard goes back there, and while it is in Git the other side is that list (O3).
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub mode: Mode,
    /// The side that has the keyboard (0 left, 1 right): in its List, in its Tree while
    /// `tree_snapshot` is there, or in a Git (`in_git`).
    pub focus: usize,
    /// The keyboard is in a git panel on the focus side — a pinned one, or an automatic one
    /// entered by Tab (O6) — of the repository whose top level this is.
    pub in_git: Option<String>,
    /// The active folder's repository (02 §3.1 `folder`'s detection): its top level when
    /// detection answered a work tree; `None` outside one, while detection runs, without git
    /// (O7).
    pub repo: Option<String>,
    /// The pinned git panel (rule 2).
    pub pin: Option<Pin>,
    /// The git panel the keyboard is in takes both sides (Cmd/Ctrl+Shift+G, 03 §9.2).
    pub git_wide: bool,
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

/// What the rules name for the two sides, whatever they show now (02 §3.1 rules 1–4).
pub fn target(inputs: &Inputs) -> [Content; 2] {
    if inputs.viewer {
        return [Content::Viewer, Content::Viewer];
    }
    // `git_wide`: the git panel with the keyboard is the only content shown.
    if let (true, Some(repo)) = (inputs.git_wide, &inputs.in_git) {
        return [Content::Git(repo.clone()), Content::Git(repo.clone())];
    }
    let other = match (
        inputs.mode,
        &inputs.in_git,
        &inputs.tree_snapshot,
        &inputs.bar,
    ) {
        // O2: dual mode — its own list.
        (Mode::Dual, ..) => Content::List,
        // O3: the keyboard is in Git — its own list, the origin list as it was.
        (Mode::Preview, Some(_), ..) => Content::List,
        // O4: the keyboard is in the tree — what the side showed when it opened.
        (Mode::Preview, None, Some(snapshot), _) => snapshot.clone(),
        // O5: the bar on a file — its preview.
        (Mode::Preview, None, None, Bar::File(path)) => Content::Preview(path.clone()),
        // O6: the bar on anything else and the active folder in a work tree — its git panel;
        // O7: otherwise its own list.
        (Mode::Preview, None, None, Bar::Other) => match &inputs.repo {
            Some(repo) => Content::Git(repo.clone()),
            None => Content::List,
        },
    };
    // Rule 3: the side with the keyboard shows what the keyboard is in.
    let own = match (&inputs.in_git, &inputs.tree_snapshot) {
        (Some(repo), _) => Content::Git(repo.clone()),
        (None, Some(_)) => Content::Tree,
        (None, None) => Content::List,
    };
    let mut sides = if inputs.focus == 0 {
        [own, other]
    } else {
        [other, own]
    };
    // Rule 2: the pinned side shows its git panel, whatever has the keyboard and whatever the
    // mode.
    if let Some(pin) = &inputs.pin {
        sides[pin.side.min(1)] = Content::Git(pin.repo.clone());
    }
    sides
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

    fn git(top: &str) -> Content {
        Content::Git(top.to_string())
    }

    /// 02 §3.1 with the git panel: every input of the state table the git panel adds — the
    /// active folder in a work tree or not, the keyboard in a git panel or not, a pin on either
    /// side — with both modes, either focus and the bar on a file or not. The pinned side always
    /// shows its panel (rule 2); the side with the keyboard shows what it is in (rule 3); the
    /// other side follows O2, O3, O5, O6, O7; and a git panel never shows on both sides.
    #[test]
    fn o6_o3_and_the_pin_name_each_sides_content() {
        use Content::{List, Preview};
        let pins = |focus: usize| {
            [
                None,
                Some(Pin {
                    side: 1 - focus,
                    repo: "/r".into(),
                }),
                Some(Pin {
                    side: focus,
                    repo: "/r".into(),
                }),
            ]
        };
        for mode in [Mode::Preview, Mode::Dual] {
            for focus in [0, 1] {
                for bar in [file("/r/a.rs"), Bar::Other] {
                    for repo in [None, Some("/r".to_string()), Some("/q".to_string())] {
                        for pin in pins(focus) {
                            // The keyboard on the pinned side is in its panel; elsewhere it may
                            // be in an automatic one (Tab into O6) or in the list.
                            let pinned_here = pin.as_ref().is_some_and(|p| p.side == focus);
                            let in_gits = if pinned_here {
                                vec![Some("/r".to_string())]
                            } else if pin.is_some() {
                                vec![None]
                            } else {
                                vec![None, repo.clone()]
                            };
                            for in_git in in_gits {
                                let inputs = Inputs {
                                    mode,
                                    focus,
                                    bar: bar.clone(),
                                    repo: repo.clone(),
                                    pin: pin.clone(),
                                    in_git: in_git.clone(),
                                    ..Default::default()
                                };
                                let got = target(&inputs);
                                let said = format!("{inputs:?}");
                                // Rule 3.
                                match &in_git {
                                    Some(top) => assert_eq!(got[focus], git(top), "{said}"),
                                    None => assert_eq!(got[focus], List, "{said}"),
                                }
                                let other = &got[1 - focus];
                                if let Some(p) = pin.as_ref().filter(|p| p.side != focus) {
                                    assert_eq!(*other, git(&p.repo), "rule 2: {said}");
                                } else if mode == Mode::Dual {
                                    assert_eq!(*other, List, "O2: {said}");
                                } else if in_git.is_some() {
                                    assert_eq!(*other, List, "O3: {said}");
                                } else if let Bar::File(path) = &bar {
                                    assert_eq!(*other, Preview(path.clone()), "O5: {said}");
                                } else if let Some(top) = &repo {
                                    assert_eq!(*other, git(top), "O6: {said}");
                                } else {
                                    assert_eq!(*other, List, "O7: {said}");
                                }
                                let gits = got.iter().filter(|c| matches!(c, Content::Git(_)));
                                assert!(gits.count() <= 1, "Git on both sides: {said}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// P8 (02 §4.5) as the rules give it: in a work tree the bar on `..` shows the git panel,
    /// on a file its preview; pinned, the panel stays while the bar moves over files; unpinned
    /// (Esc), the preview again.
    #[test]
    fn p8_the_bar_on_a_folder_shows_git_and_the_pin_keeps_it() {
        let at = |bar: Bar, pin: Option<Pin>| Inputs {
            bar,
            repo: Some("/r".into()),
            pin,
            ..Default::default()
        };
        assert_eq!(target(&at(Bar::Other, None))[1], git("/r"));
        assert_eq!(target(&at(file("/r/a.rs"), None))[1], preview("/r/a.rs"));
        let pin = Some(Pin {
            side: 1,
            repo: "/r".into(),
        });
        for bar in [file("/r/a.rs"), file("/r/b.txt"), Bar::Other] {
            assert_eq!(target(&at(bar, pin.clone()))[1], git("/r"));
        }
        // Navigating with a pin: outside the repo the pinned panel stays (its title says which).
        let outside = Inputs {
            repo: None,
            ..at(Bar::Other, pin.clone())
        };
        assert_eq!(target(&outside), [Content::List, git("/r")]);
        assert_eq!(target(&at(file("/r/a.rs"), None))[1], preview("/r/a.rs"));
    }

    /// P9, P10 and Cmd/Ctrl+Shift+G (02 §3.2): Tab into a pinned or an automatic git panel —
    /// the other side is the origin list (O3), not Git; dual mode keeps the pinned side's Git
    /// and the other side a list; wide, the git panel is the only content.
    #[test]
    fn p9_p10_the_keyboard_in_git_and_wide() {
        let pinned = Inputs {
            focus: 1,
            in_git: Some("/r".into()),
            pin: Some(Pin {
                side: 1,
                repo: "/r".into(),
            }),
            repo: Some("/r".into()),
            ..Default::default()
        };
        assert_eq!(target(&pinned), [Content::List, git("/r")]);
        let dual = Inputs {
            mode: Mode::Dual,
            ..pinned.clone()
        };
        assert_eq!(target(&dual), [Content::List, git("/r")], "P7 with a pin");
        // An automatic panel entered by Tab (no pin): the same, until F9 (P10) gives the
        // keyboard back to the origin list — then both sides are lists.
        let automatic = Inputs {
            pin: None,
            ..pinned.clone()
        };
        assert_eq!(target(&automatic), [Content::List, git("/r")]);
        let back = Inputs {
            mode: Mode::Dual,
            focus: 0,
            in_git: None,
            ..automatic.clone()
        };
        assert_eq!(target(&back), [Content::List, Content::List], "P10");
        let wide = Inputs {
            git_wide: true,
            ..automatic
        };
        assert_eq!(target(&wide), [git("/r"), git("/r")]);
        // Wide is the keyboard's panel: nothing to widen from a list.
        let list = Inputs {
            git_wide: true,
            in_git: None,
            focus: 0,
            ..pinned
        };
        assert_eq!(target(&list), [Content::List, git("/r")]);
    }

    /// The git panel follows the bar's rest like a preview (02 §3.1 "Committing"): the bar
    /// leaving `..` for a file keeps the git panel until 150 ms, and a detection answer — not
    /// a bar move — commits at once.
    #[test]
    fn the_git_panel_waits_for_the_bars_rest_like_a_preview() {
        let shown = [Content::List, git("/r")];
        let moved = Inputs {
            bar: file("/r/a.rs"),
            repo: Some("/r".into()),
            rest: Some(MS(40)),
            shown: Some(shown.clone()),
            ..Default::default()
        };
        assert_eq!(resolve(&moved).show, shown);
        assert_eq!(resolve(&moved).pending, Some(MS(110)));
        let answered = Inputs {
            bar: Bar::Other,
            repo: Some("/r".into()),
            rest: None,
            shown: Some([Content::List, Content::List]),
            ..Default::default()
        };
        assert_eq!(resolve(&answered).show, shown);
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
