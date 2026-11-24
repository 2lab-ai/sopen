//! F1's key help (04 §2 slot 1, §6.1): a read-only screen listing the keys of where the keyboard
//! is — the list, a viewer, the git panel, the tree — in every xtools window, in the skin of
//! [`super::chrome`].
//!
//! - **What it lists** comes from the browser's one key table ([`for_context`]: its landed rows
//!   of that context, the F-key slots a hosted viewer adds) or, in a window of its own (image,
//!   media, hex, the binary dialog), from that window's F-key row ([`for_window`]). Both end with
//!   the Cmd/Ctrl+digit aliases and the clickable row (04 §6.1).
//! - **Who opens it**: a window of its own on F1 ([`super::own_keys`]); the browser on
//!   `keys::Command::Help` with its context's page (S1-E). [`open`] puts it on screen.
//! - **While it is open** it covers the window and takes the keyboard and the pointer: Esc, F1
//!   or F3 close it, ↑ ↓ PgUp PgDn Home End and the wheel scroll it, and the window's pass sees
//!   no key, typed text, wheel or pointer (Cmd/Ctrl+Q and +W close the help and go on to close
//!   the window, through its unsaved-changes prompt). The
//!   shell's input plugin (`super::FKeyInput`) runs [`begin_pass`] before every window's pass
//!   and [`end_pass`] after it. The screen is painted, not laid out: a plugin's pass must not
//!   make widgets (egui's debug build asks every plugin about the widget under the pointer,
//!   the one running included, and waits on its lock for ever).

use super::browser::keys::{self, Context, Mods, Trigger};
use super::browser::view::Mode;
use super::chrome::{self, FKey, CELL};
use eframe::egui::{self, Event, Key, Pos2, Rect, Vec2};

/// One line of the screen: the keys (alternatives joined by ", ") and what they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub keys: String,
    pub what: String,
}

/// What the screen shows: its title (in the frame's top border) and its lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Page {
    pub title: String,
    pub lines: Vec<Line>,
}

/// The browser's keys in `context` (its landed rows, 02 §7), then the F-key slots of `own` that
/// the table has no row for there (a hosted viewer's own keys: the hex window's 5 Save, …), then
/// the aliases. F9's words name the mode it switches to in `mode`. The browser opens it on
/// `keys::Command::Help`.
pub(crate) fn for_context(context: Context, mode: Mode, own: &[FKey]) -> Page {
    page_upto(context, mode, own, keys::LANDED_SLICES)
}

/// [`for_context`] with the rows of the slices in `upto` — the set of landed slices
/// ([`keys::LANDED_SLICES`]; a slice's integration WU adds its number), or every slice in the
/// tests.
fn page_upto(context: Context, mode: Mode, own: &[FKey], upto: keys::Slices) -> Page {
    // One line per wording, in the table's order; its keys plain ones first.
    let mut lines: Vec<(Vec<(bool, String)>, String)> = Vec::new();
    let mut slots = Vec::new();
    for row in keys::rows(context, upto) {
        let plain = match row.trigger {
            Trigger::Key(key, Mods::None) => {
                slots.extend(chrome::fkey_number(key));
                true
            }
            Trigger::Key(..) => false,
            Trigger::Char(_) | Trigger::Letter => true,
        };
        let what = if row.command == keys::Command::SwitchMode {
            switch_what(mode).to_string()
        } else {
            row.what.to_string()
        };
        let name = (plain, row.trigger.name());
        match lines.iter_mut().find(|(_, w)| *w == what) {
            Some((names, _)) => names.push(name),
            None => lines.push((vec![name], what)),
        }
    }
    let mut out: Vec<Line> = lines
        .into_iter()
        .map(|(mut names, what)| {
            names.sort_by_key(|(plain, _)| !plain);
            let names: Vec<String> = names.into_iter().map(|(_, name)| name).collect();
            Line {
                keys: names.join(", "),
                what,
            }
        })
        .collect();
    // The slots a hosted window adds, where the table has no F-key of its own.
    let mut own: Vec<FKey> = own
        .iter()
        .filter(|k| !slots.contains(&k.n))
        .copied()
        .collect();
    own.sort_by_key(|k| k.n);
    own.dedup_by_key(|k| k.n);
    out.extend(own.into_iter().map(own_line));
    out.push(aliases());
    Page {
        title: format!("Keys in {}", context.name()),
        lines: out,
    }
}

/// What F9 does in `mode` (the mode it switches to, as its label says).
fn switch_what(mode: Mode) -> &'static str {
    match mode {
        Mode::Preview => "Switch to dual mode: two lists",
        Mode::Dual => "Switch to preview mode: the other side follows the bar",
    }
}

/// A window of its own: its F-key row's slots (`row`, as [`super::own_keys`] draws it), then
/// the aliases.
pub(crate) fn for_window(row: &[FKey]) -> Page {
    let mut row = row.to_vec();
    row.sort_by_key(|k| k.n);
    let mut lines: Vec<Line> = row.into_iter().map(own_line).collect();
    lines.push(aliases());
    Page {
        title: "Keys".to_string(),
        lines,
    }
}

/// The line of a window's own F-key.
fn own_line(key: FKey) -> Line {
    Line {
        keys: format!("F{}", key.n),
        what: describe(key),
    }
}

/// The last line of every page: Cmd/Ctrl+digits and the row's slots act as F-keys (04 §6.1).
fn aliases() -> Line {
    Line {
        keys: format!("{}+1…9, 0", keys::command_name()),
        what: "F1…F10 (so does a click on a slot)".to_string(),
    }
}

/// What a window's own F-key does, from its row's label (the viewers' keys, 01 §3a).
fn describe(key: FKey) -> String {
    let cmd = keys::command_name();
    match (key.n, key.label) {
        (1, "Help") => "This screen".to_string(),
        (3, "Exit") => format!("Close the window (as {cmd}+W)"),
        (5, "Save") => format!("Save (as {cmd}+S)"),
        (6, "Goto") => format!("Go to an offset (as {cmd}+G)"),
        (7, "SaveAs") => format!("Save As: a new local file with your edits (as Alt+{cmd}+S)"),
        (8, "Asc/hx") => "Type in the ASCII or the hex column (as Tab)".to_string(),
        (2, "Unwrap") => "One row per line (as W)".to_string(),
        (2, "Wrap") => "Wrap the lines at the window's width (as W)".to_string(),
        (_, label) => label.to_string(),
    }
}

/// The screen while it is open: its page and the first line shown.
#[derive(Debug, Clone, Default)]
struct Open {
    page: Page,
    top: usize,
    /// Lines that fit, as the last pass drew them (a page for PgUp / PgDn).
    rows: usize,
}

fn open_id() -> egui::Id {
    egui::Id::unique("sopen-key-help")
}

/// Shows `page` over the window, from the next pass on (and this one's end).
pub(crate) fn open(ctx: &egui::Context, page: Page) {
    ctx.data_mut(|d| {
        d.insert_temp(
            open_id(),
            Open {
                page,
                top: 0,
                rows: 0,
            },
        )
    });
    ctx.request_repaint();
}

/// Whether the help screen is on screen.
#[cfg(test)]
pub(crate) fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Open>(open_id()).is_some())
}

/// Before the window's pass: the screen's own keys, then the rest of the frame's input withheld
/// from the window — every key but Cmd/Ctrl+Q and +W (they close it as ever), the typed text,
/// the wheel and the pointer (egui has already worked out the pointer's state for this pass from
/// its events, so that state goes too, and nothing under the screen is hovered or clicked).
pub(crate) fn begin_pass(ctx: &egui::Context) {
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(open_id())) else {
        return;
    };
    let page = open.rows.saturating_sub(1).max(1);
    let last = open.page.lines.len().saturating_sub(open.rows);
    let closed = ctx.input_mut(|i| {
        let mut closed = false;
        for key in [Key::Escape, Key::F1, Key::F3] {
            closed |= keys::take(i, key, Mods::None);
        }
        // Cmd/Ctrl+Q or +W goes on to the window, which may ask about unsaved changes: the
        // help goes first, so its question is on screen.
        closed |= i.events.iter().any(|e| {
            matches!(e, Event::Key { key: Key::Q | Key::W, pressed: true, modifiers, .. }
                if Mods::Command.matches(*modifiers))
        });
        let steps: [(Key, isize); 4] = [
            (Key::ArrowDown, 1),
            (Key::ArrowUp, -1),
            (Key::PageDown, page as isize),
            (Key::PageUp, -(page as isize)),
        ];
        for (key, by) in steps {
            while keys::take(i, key, Mods::None) {
                open.top = open.top.saturating_add_signed(by).min(last);
            }
        }
        if keys::take(i, Key::Home, Mods::None) {
            open.top = 0;
        }
        if keys::take(i, Key::End, Mods::None) {
            open.top = last;
        }
        let lines: f32 = i
            .events
            .iter()
            .map(|e| match e {
                Event::MouseWheel { unit, delta, .. } => match unit {
                    egui::MouseWheelUnit::Point => -delta.y / CELL.y,
                    _ => -delta.y * 3.0,
                },
                _ => 0.0,
            })
            .sum();
        open.top = (open.top as f32 + lines).round().clamp(0.0, last as f32) as usize;
        i.events.retain(|e| match e {
            Event::Key { key, modifiers, .. } => {
                matches!(key, Key::Q | Key::W) && Mods::Command.matches(*modifiers)
            }
            Event::ModifiersChanged(_) | Event::WindowFocused(_) => true,
            _ => false,
        });
        i.pointer = Default::default();
        i.smooth_scroll_delta = Vec2::ZERO;
        closed
    });
    ctx.data_mut(|d| {
        if closed {
            d.remove_temp::<Open>(open_id());
        } else {
            d.insert_temp(open_id(), open);
        }
    });
}

/// After the window's pass: the screen over the whole window, in the window look — the page's
/// title in the frame, a key column and a column of words, the scroll bar when they do not all
/// fit, and how to leave it on the last row.
pub(crate) fn end_pass(ctx: &egui::Context) {
    let Some(mut open) = ctx.data(|d| d.get_temp::<Open>(open_id())) else {
        return;
    };
    let p = chrome::PALETTE;
    let screen = ctx.content_rect();
    // Above everything a window draws, its dialogs included.
    let painter = ctx
        .layer_painter(egui::LayerId::new(egui::Order::Tooltip, open_id()))
        .with_clip_rect(screen);
    painter.rect_filled(screen, 0.0, p.back);
    let inner = chrome::frame(&painter, screen, &open.page.title, true);
    let font = chrome::font();
    // The last row says how to leave; a cell's gap above it.
    let rows = (((inner.height() / CELL.y).floor() as usize).saturating_sub(2)).max(1);
    open.rows = rows;
    let last = open.page.lines.len().saturating_sub(rows);
    open.top = open.top.min(last);
    let widest = open
        .page
        .lines
        .iter()
        .map(|l| l.keys.chars().count())
        .max()
        .unwrap_or(0)
        .min(28);
    let keys_x = inner.left() + CELL.x;
    let what_x = keys_x + CELL.x * (widest + 2) as f32;
    let room = (inner.right() - CELL.x - what_x).max(0.0);
    for (k, line) in open.page.lines.iter().skip(open.top).take(rows).enumerate() {
        let y = inner.top() + CELL.y * k as f32;
        let keys_room =
            Rect::from_min_max(Pos2::new(keys_x, y), Pos2::new(what_x - CELL.x, y + CELL.y));
        let shown = chrome::fit_start(ctx, &line.keys, &font, keys_room.width());
        painter.with_clip_rect(keys_room).text(
            keys_room.min,
            egui::Align2::LEFT_TOP,
            shown,
            font.clone(),
            p.help_key,
        );
        painter.text(
            Pos2::new(what_x, y),
            egui::Align2::LEFT_TOP,
            chrome::fit_start(ctx, &line.what, &font, room),
            font.clone(),
            p.text,
        );
    }
    let more = open.page.lines.len() > rows;
    let foot = if more {
        "Esc, F1 or F3: back · ↑ ↓ PgUp PgDn: more"
    } else {
        "Esc, F1 or F3: back"
    };
    painter.text(
        Pos2::new(keys_x, inner.bottom() - CELL.y),
        egui::Align2::LEFT_TOP,
        chrome::fit_start(ctx, foot, &font, (inner.right() - keys_x).max(0.0)),
        font.clone(),
        p.dim,
    );
    if more {
        let column = Rect::from_min_size(
            Pos2::new(inner.right(), inner.top()),
            Vec2::new(CELL.x, CELL.y * rows as f32),
        );
        chrome::scroll_bar(&painter, column, Some(open.top as f32 / last.max(1) as f32));
    }
    ctx.data_mut(|d| d.insert_temp(open_id(), open));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::browser::keys::{rows, LANDED_SLICES};
    use crate::gui::testkit::Driver;
    use crate::gui::Window;

    fn line<'a>(page: &'a Page, keys: &str) -> Option<&'a Line> {
        page.lines.iter().find(|l| l.keys == keys)
    }

    /// F1 in the list (K-R1's help): every landed row of the list is on one line, the
    /// alternatives of one action together (plain key first), in the table's order — S6's F7 and
    /// F8 among them; the aliases last.
    #[test]
    fn the_list_page_lists_the_tables_rows() {
        let page = for_context(Context::List, Mode::Preview, &[]);
        assert_eq!(page.title, "Keys in the file list");
        let cmd = keys::command_name();
        for (keys, what) in [
            ("F1", "This screen"),
            ("F2, Shift+F6", "Rename the entry under the bar, in place"),
            ("F3", "The file under the bar in its viewer"),
            ("F5", "Copy to the other panel's folder (asks first)"),
            ("F6", "Move to the other panel's folder (asks first)"),
            ("Tab, Shift+Tab", "The keyboard to the other side"),
            ("↑, ↓, PgUp, PgDn, Home, End", "Move the bar"),
            (
                "Space, Ins",
                "Select or unselect the entry under the bar, then step down",
            ),
            ("+", "Select the entries matching a pattern"),
            ("*", "Invert the selection"),
            ("a letter", "The next name starting with it"),
        ] {
            assert_eq!(
                line(&page, keys).map(|l| l.what.as_str()),
                Some(what),
                "{keys}: {page:#?}"
            );
        }
        assert!(
            line(&page, &format!("Backspace, {cmd}+PgUp")).is_some(),
            "{page:#?}"
        );
        assert!(line(&page, &format!("{cmd}+A")).is_some());
        assert!(line(&page, "F9")
            .unwrap()
            .what
            .starts_with("Switch to dual mode"));
        let dual = for_context(Context::List, Mode::Dual, &[]);
        assert!(line(&dual, "F9")
            .unwrap()
            .what
            .starts_with("Switch to preview mode"));
        // Each landed row's key is on exactly one line.
        let all: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
        for row in rows(Context::List, LANDED_SLICES) {
            let name = row.trigger.name();
            let hits = all
                .iter()
                .filter(|keys| keys.split(", ").any(|k| k == name))
                .count();
            assert_eq!(hits, 1, "{name} in {all:?}");
        }
        // S5 has landed: F10 and Alt+F10, the tree, on one line.
        assert_eq!(
            line(&page, "F10, Alt+F10").map(|l| l.what.as_str()),
            Some("The directory tree in place of this list"),
            "{page:#?}"
        );
        // S2 has landed: Cmd/Ctrl+G pins the git panel.
        assert_eq!(
            line(&page, &format!("{cmd}+G")).map(|l| l.what.as_str()),
            Some("Pin the git panel to the other side; again: unpin"),
            "{page:#?}"
        );
        // S6 has landed: F7 makes a folder; F8, Del (and ⌘⌫ on macOS) delete to the Trash,
        // Shift+F8 and Shift+Del without it — each action on one line.
        assert_eq!(
            line(&page, "F7").map(|l| l.what.as_str()),
            Some("Make a folder (asks its name)"),
            "{page:#?}"
        );
        let delete = match cfg!(target_os = "macos") {
            true => format!("F8, Del, {cmd}+Backspace"),
            false => "F8, Del".to_string(),
        };
        assert_eq!(
            line(&page, &delete).map(|l| l.what.as_str()),
            Some("Delete the selection or the entry (asks; locally to the Trash)"),
            "{page:#?}"
        );
        assert_eq!(
            line(&page, "Shift+F8, Shift+Del").map(|l| l.what.as_str()),
            Some("Delete without the Trash (asks)"),
            "{page:#?}"
        );
        assert_eq!(page.lines.last(), Some(&aliases()));
        assert_eq!(page.lines.first().map(|l| l.keys.as_str()), Some("F1"));
    }

    /// F1 in the tree (05 §1, landed with S5): every key of the tree's context, the aliases last.
    #[test]
    fn the_tree_page_lists_the_trees_keys() {
        let page = for_context(Context::Tree, Mode::Preview, &[]);
        assert_eq!(page.title, "Keys in the directory tree");
        let cmd = keys::command_name();
        for (keys, what) in [
            ("F1", "This screen"),
            ("F10, Esc, Alt+F10", "Close the tree, the list as it was"),
            ("Enter", "List that folder in this panel"),
            ("→", "Open the folder (listed if it never was)"),
            ("←", "Close the folder; on a closed one: its parent"),
            ("↑, ↓, PgUp, PgDn, Home, End", "Move the bar"),
            ("a letter", "The next folder starting with it"),
            ("Space", "Size the folder now"),
            (&format!("{cmd}+R"), "List the folder again"),
        ] {
            assert_eq!(
                line(&page, keys).map(|l| l.what.as_str()),
                Some(what),
                "{keys}: {page:#?}"
            );
        }
        assert_eq!(page.lines.last(), Some(&aliases()));
    }

    /// F1 in the git panel (03 §9; S2's keys, S3's and S4's since they landed), each on one
    /// line: the details, a file at a revision, the editor, the mode, the bar, the label cursor,
    /// marks and staging, Tab back, Esc, Find, history search, refresh, the filters, HEAD, wide,
    /// the pin; S3's slots (Pick, Rebase, Branch / Commit, the menu), Fetch all, undo, the
    /// Backups list, the command log and the banner's Cmd/Ctrl+Shift+C, S, A, B; S4's F2 (a
    /// commit's reword, a label's rename) and F8 (a commit's drop among the deletes), the
    /// planner's Cmd/Ctrl+E and an edit stop's Cmd/Ctrl+Shift+M and P (03 §9.2).
    #[test]
    fn the_git_page_lists_the_git_panels_keys() {
        let page = for_context(Context::Git, Mode::Preview, &[]);
        assert_eq!(page.title, "Keys in the git panel");
        let cmd = keys::command_name();
        for (keys, what) in [
            ("F1", "This screen"),
            ("Shift+F3", "The file at this revision"),
            (
                "F3, Enter",
                "The row's details (with one marked row: compare the two)",
            ),
            (
                "F4",
                "The repository (uncommitted row: the file) in the editor",
            ),
            ("↑, ↓, PgUp, PgDn, Home, End", "Move the bar"),
            ("←", "The label cursor to the previous label"),
            ("→", "The label cursor to the next label"),
            (
                "Space, Ins",
                "Mark or unmark the row, then step down (a changed file: stage or unstage it)",
            ),
            ("Tab, Shift+Tab", "Back to the list"),
            ("Esc", "Close the innermost open thing; with none, unpin"),
            ("a letter", "Find, starting with it"),
            (&format!("{cmd}+Shift+F"), "History search"),
            (&format!("{cmd}+F"), "Find"),
            (&format!("{cmd}+R"), "Refresh (hard)"),
            (&format!("{cmd}+J"), "The bar to HEAD"),
            (
                &format!("{cmd}+Shift+G"),
                "The git panel over both sides, and back",
            ),
            (&format!("{cmd}+G"), "Pin this git panel; again: unpin"),
            (&format!("{cmd}+K"), "The filters"),
            // S3, and S4's F2.
            ("F2", "A commit: reword it; a branch's label: rename it"),
            ("F5", "Cherry-pick it or the marked ones; a stash: apply it"),
            ("F6", "Rebase the current branch onto it"),
            ("F7", "A new branch here (uncommitted row: commit)"),
            ("F8", "Delete, drop or discard what is under the bar (asks)"),
            ("F10, Shift+F10", "The menu of what is under the bar"),
            (&format!("{cmd}+Shift+R"), "Fetch all"),
            (&format!("{cmd}+Z"), "Undo the newest backup group"),
            (&format!("{cmd}+B"), "The backups"),
            (&format!("{cmd}+L"), "The command log"),
            (
                &format!("{cmd}+Shift+C"),
                "The operation in progress: Continue (asks)",
            ),
            (
                &format!("{cmd}+Shift+S"),
                "The operation in progress: Skip (asks)",
            ),
            (
                &format!("{cmd}+Shift+A"),
                "The operation in progress: Abort (asks)",
            ),
            (
                &format!("{cmd}+Shift+B"),
                "Bisect in progress: Reset (asks)",
            ),
            // S4.
            (
                &format!("{cmd}+E"),
                "Plan a history edit from the commit under the bar",
            ),
            (&format!("{cmd}+Shift+M"), "An edit stop: Amend (asks)"),
            (&format!("{cmd}+Shift+P"), "An edit stop: Split (asks)"),
        ] {
            assert_eq!(
                line(&page, keys).map(|l| l.what.as_str()),
                Some(what),
                "{keys}: {page:#?}"
            );
        }
        assert!(line(&page, "F9")
            .unwrap()
            .what
            .starts_with("Switch to dual mode"));
        assert_eq!(page.lines.last(), Some(&aliases()));
    }

    /// A viewer in the browser: the table's viewer rows, then the viewer's own slots the table
    /// has no row for (the hex window's), described.
    #[test]
    fn the_viewer_page_adds_the_viewers_own_keys() {
        let mut own = vec![chrome::EXIT_KEY];
        own.extend_from_slice(chrome::HEX_KEYS);
        let page = for_context(Context::Viewer, Mode::Preview, &own);
        let keys: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
        assert_eq!(
            keys,
            [
                "F1",
                "F3, Esc",
                "F4",
                "F5",
                "F6",
                "F7",
                "F8",
                &aliases().keys
            ]
        );
        let cmd = keys::command_name();
        assert_eq!(
            line(&page, "F5").unwrap().what,
            format!("Save (as {cmd}+S)")
        );
        assert_eq!(line(&page, "F3, Esc").unwrap().what, "Back to the list");
        assert_eq!(
            line(&page, "F7").unwrap().what,
            format!("Save As: a new local file with your edits (as Alt+{cmd}+S)")
        );
    }

    /// A window of its own lists its row: Help, Exit (as Cmd/Ctrl+W) and its own keys.
    #[test]
    fn a_window_page_lists_its_row() {
        let row = chrome::merged(&[chrome::HELP_KEY, chrome::EXIT_KEY], chrome::IMAGE_KEYS);
        let page = for_window(&row);
        assert_eq!(page.title, "Keys");
        let cmd = keys::command_name();
        assert_eq!(
            page.lines,
            [
                Line {
                    keys: "F1".into(),
                    what: "This screen".into()
                },
                Line {
                    keys: "F3".into(),
                    what: format!("Close the window (as {cmd}+W)")
                },
                Line {
                    keys: "F5".into(),
                    what: format!("Save (as {cmd}+S)")
                },
                Line {
                    keys: "F7".into(),
                    what: format!("Save As: a new local file with your edits (as Alt+{cmd}+S)")
                },
                aliases(),
            ]
        );
    }

    /// Every context's page, with every slice landed, holds every row once (S2–S5 open their
    /// contexts' help with this).
    #[test]
    fn every_contexts_page_covers_its_rows() {
        for context in Context::ALL {
            let page = page_upto(context, Mode::Preview, &[], keys::ALL_SLICES);
            let all: Vec<&str> = page.lines.iter().map(|l| l.keys.as_str()).collect();
            for row in rows(context, keys::ALL_SLICES) {
                let name = row.trigger.name();
                let hits = all
                    .iter()
                    .filter(|keys| keys.split(", ").any(|k| k == name))
                    .count();
                assert_eq!(hits, 1, "{context:?}: {name} in {all:?}");
            }
            if rows(context, keys::ALL_SLICES).any(|r| r.trigger == Trigger::Letter) {
                assert!(all.contains(&"a letter"), "{context:?}");
            }
            assert_eq!(page.lines.last(), Some(&aliases()), "{context:?}");
        }
    }

    /// Opens `page` on its first pass, over a label.
    struct Shows {
        page: Option<Page>,
    }

    impl Window for Shows {
        fn ui(&mut self, ui: &mut egui::Ui) {
            if let Some(page) = self.page.take() {
                open(ui.ctx(), page);
            }
            ui.label("under the help");
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// The screen: the page's title in the frame, each line's keys in the key colour with its
    /// words beside them on the same row; in a short window the rest comes with End, PgDn and
    /// ↓, and goes back with Home.
    #[test]
    fn the_screen_draws_the_page_and_scrolls() {
        let page = for_context(Context::List, Mode::Preview, &[]);
        let mut drv = Driver::new(640.0, 240.0);
        let mut win = Shows {
            page: Some(page.clone()),
        };
        drv.idle(&mut win, 2);
        assert!(is_open(&drv.ctx));
        assert!(
            drv.find_text("Keys in the file list").is_some(),
            "the title"
        );
        let keys = drv.find_text("F2, Shift+F6").expect("the rename line");
        let words = drv
            .find_text("Rename the entry under the bar, in place")
            .expect("its words");
        assert_eq!(keys.top(), words.top(), "one row");
        assert!(keys.right() < words.left());
        assert_eq!(
            drv.colors_of("F2, Shift+F6"),
            Some(vec![chrome::PALETTE.help_key])
        );
        let alias = aliases().keys;
        assert!(drv.find_text(&alias).is_none(), "below the screen");
        assert!(drv
            .find_text("Esc, F1 or F3: back · ↑ ↓ PgUp PgDn: more")
            .is_some());
        drv.key(&mut win, Key::End, egui::Modifiers::NONE);
        assert!(drv.find_text(&alias).is_some(), "End: the last line");
        assert!(drv.find_text("This screen").is_none());
        drv.key(&mut win, Key::Home, egui::Modifiers::NONE);
        assert!(drv.find_text("This screen").is_some());
        drv.key(&mut win, Key::ArrowDown, egui::Modifiers::NONE);
        assert!(drv.find_text("This screen").is_none(), "↓: a line on");
        drv.key(&mut win, Key::PageUp, egui::Modifiers::NONE);
        assert!(drv.find_text("This screen").is_some());
        drv.key(&mut win, Key::Escape, egui::Modifiers::NONE);
        assert!(!is_open(&drv.ctx));
        assert!(drv.find_text("Keys in the file list").is_none());
        assert!(drv.find_text("under the help").is_some());
    }
}
