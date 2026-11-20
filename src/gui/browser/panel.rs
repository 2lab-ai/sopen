//! One file panel: the folder it shows, its entries in the browser's order (`..` first, then
//! folders, then files, each by name), the highlight bar and the first row on screen, the
//! selection (04 §4) — and how a row and the footer read (`N selected = X bytes` while anything
//! is selected, `N Listed = N bytes`, then `N Bytes Free`, in PC Shell's words).

use crate::remote::{Entry, EntryKind};
use std::collections::HashSet;
use std::time::SystemTime;

/// The entry that leads to the parent folder.
pub const UP: &str = "..";

pub struct Panel {
    /// The folder shown (absolute).
    pub dir: String,
    pub entries: Vec<Entry>,
    /// The entry under the highlight bar, and the first row on screen.
    pub highlight: usize,
    pub top: usize,
    /// Bytes free where the folder is, when the listing could say.
    pub free: Option<u64>,
    /// The bar moved since the rows were last scrolled to it (the wheel scrolls freely).
    moved: bool,
    /// The selected entries, by name (04 §4): never `..`, never a name that is not UTF-8
    /// ([`selectable`]). A re-read of the same folder keeps the names still there; another
    /// folder clears it ([`Panel::fill`]).
    selected: HashSet<String>,
}

impl Panel {
    pub fn new(dir: &str) -> Self {
        Self {
            dir: dir.to_string(),
            entries: Vec::new(),
            highlight: 0,
            top: 0,
            free: None,
            moved: true,
            selected: HashSet::new(),
        }
    }

    /// Shows `dir` with its `entries`, sorted, `..` first unless it is the root; the highlight
    /// on `select` (the folder just left, going up) or on the first row. The selection keeps the
    /// names still there when `dir` is the folder shown (a re-read), and is cleared otherwise
    /// (04 §4).
    pub fn fill(
        &mut self,
        dir: String,
        mut entries: Vec<Entry>,
        free: Option<u64>,
        select: Option<&str>,
    ) {
        sort(&mut entries);
        if dir != "/" {
            entries.insert(
                0,
                Entry {
                    name: UP.to_string(),
                    lossy: false,
                    kind: EntryKind::Dir,
                    link: false,
                    size: 0,
                    mtime: SystemTime::UNIX_EPOCH,
                },
            );
        }
        self.highlight = select
            .and_then(|name| entries.iter().position(|e| e.name == name && !e.lossy))
            .unwrap_or(0);
        self.top = 0;
        self.moved = true;
        if dir == self.dir {
            let there: HashSet<&str> = entries
                .iter()
                .filter(|e| selectable(e))
                .map(|e| e.name.as_str())
                .collect();
            self.selected.retain(|name| there.contains(name.as_str()));
        } else {
            self.selected.clear();
        }
        self.dir = dir;
        self.entries = entries;
        self.free = free;
    }

    /// The entry under the bar.
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.highlight)
    }

    /// Moves the bar by `delta` rows, stopping at the first and the last.
    pub fn step(&mut self, delta: isize) {
        let last = self.entries.len().saturating_sub(1);
        self.highlight = self.highlight.saturating_add_signed(delta).min(last);
        self.moved = true;
    }

    pub fn home(&mut self) {
        self.highlight = 0;
        self.moved = true;
    }

    pub fn end(&mut self) {
        self.highlight = self.entries.len().saturating_sub(1);
        self.moved = true;
    }

    /// Moves the bar to the next entry after it whose name starts with `c` (any case), around
    /// the end; whether there was one.
    pub fn jump(&mut self, c: char) -> bool {
        let n = self.entries.len();
        let wanted: String = c.to_lowercase().collect();
        for k in 1..=n {
            let i = (self.highlight + k) % n;
            let name = &self.entries[i].name;
            if name != UP && name.to_lowercase().starts_with(&wanted) {
                self.highlight = i;
                self.moved = true;
                return true;
            }
        }
        false
    }

    /// Scrolls so the bar is on screen, `visible` rows at a time, if it moved since the last
    /// time (a wheel scroll in between is left alone).
    pub fn follow(&mut self, visible: usize) {
        if !std::mem::take(&mut self.moved) {
            return;
        }
        let visible = visible.max(1);
        if self.highlight < self.top {
            self.top = self.highlight;
        } else if self.highlight >= self.top + visible {
            self.top = self.highlight + 1 - visible;
        }
        self.top = self.top.min(self.entries.len().saturating_sub(visible));
    }

    /// The path of `entry` in this panel's folder (its parent for `..`).
    pub fn path_of(&self, entry: &Entry) -> String {
        if entry.name == UP {
            crate::remote::split_parent(&self.dir).0.to_string()
        } else {
            crate::remote::join(&self.dir, &entry.name)
        }
    }

    /// Whether `entry` is selected (drawn in the selection's colour, 04 §4).
    pub fn is_selected(&self, entry: &Entry) -> bool {
        selectable(entry) && self.selected.contains(&entry.name)
    }

    /// Space and Ins: selects the bar's entry, or unselects it, and moves the bar one row down
    /// (`..` and a name that is not UTF-8 are never selected: the bar only moves).
    pub fn toggle(&mut self) {
        if let Some(entry) = self.current().filter(|e| selectable(e)) {
            let name = entry.name.clone();
            if !self.selected.remove(&name) {
                self.selected.insert(name);
            }
        }
        self.step(1);
    }

    /// Cmd/Ctrl+A: every entry that can be selected.
    pub fn select_all(&mut self) {
        let all = self.entries.iter().filter(|e| selectable(e));
        self.selected = all.map(|e| e.name.clone()).collect();
    }

    /// `*`: every entry that can be selected changes, files and folders alike.
    pub fn invert(&mut self) {
        let all = self.entries.iter().filter(|e| selectable(e));
        self.selected = all
            .filter(|e| !self.selected.contains(&e.name))
            .map(|e| e.name.clone())
            .collect();
    }

    /// `+` (`on`) and `-`: selects or unselects the entries whose name matches `pattern`
    /// ([`matches`]), files and folders alike; how many matched.
    pub fn select_group(&mut self, pattern: &str, on: bool) -> usize {
        let hits: Vec<String> = self
            .entries
            .iter()
            .filter(|e| selectable(e) && matches(pattern, &e.name))
            .map(|e| e.name.clone())
            .collect();
        let n = hits.len();
        for name in hits {
            if on {
                self.selected.insert(name);
            } else {
                self.selected.remove(&name);
            }
        }
        n
    }

    /// Unselects the entry named `name` (an item an operation has done).
    pub fn unselect(&mut self, name: &str) {
        self.selected.remove(name);
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// The selected entries, in the panel's order.
    pub fn selection(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| self.is_selected(e))
            .collect()
    }

    /// What Copy, Move and Delete act on (04 §4): the selection when there is one, else the
    /// bar's entry — none when that is `..` or a name that is not UTF-8.
    pub fn acted_on(&self) -> Vec<&Entry> {
        let selection = self.selection();
        if !selection.is_empty() {
            return selection;
        }
        self.current()
            .filter(|e| selectable(e))
            .into_iter()
            .collect()
    }

    /// `N selected = X bytes` while anything is selected: the files' bytes, folders counted as
    /// entries (04 §4).
    pub fn selected_foot(&self) -> Option<String> {
        let selection = self.selection();
        if selection.is_empty() {
            return None;
        }
        let bytes: u128 = selection
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| u128::from(e.size))
            .sum();
        Some(format!(
            "{} selected = {} bytes",
            grouped(selection.len() as u128),
            grouped(bytes)
        ))
    }

    /// `N Listed = N bytes` (the entries but `..`, the bytes of the files), then the free space
    /// as `N Bytes Free` — PC Shell's two window feet on one line.
    pub fn footer(&self) -> String {
        match self.foot_parts() {
            (listed, Some(free)) => format!("{listed} · {free}"),
            (listed, None) => listed,
        }
    }

    /// The footer's two parts: the counts — `N selected = X bytes · ` first while anything is
    /// selected (04 §4), then `N Listed = N bytes` — and `N Bytes Free` when the listing said.
    /// The browser drops the free space first when the foot does not fit, then cuts the counts'
    /// end: the selection is what stays in sight.
    pub fn foot_parts(&self) -> (String, Option<String>) {
        let listed: Vec<&Entry> = self.entries.iter().filter(|e| e.name != UP).collect();
        let bytes: u128 = listed
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| u128::from(e.size))
            .sum();
        let count = format!(
            "{} Listed = {} bytes",
            grouped(listed.len() as u128),
            grouped(bytes)
        );
        let free = self
            .free
            .map(|free| format!("{} Bytes Free", grouped(u128::from(free))));
        match self.selected_foot() {
            Some(selected) => (format!("{selected} · {count}"), free),
            None => (count, free),
        }
    }
}

/// `1234567` → `1,234,567`.
fn grouped(n: u128) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Folders (and links to folders) first, then everything else; each by name regardless of
/// case, then byte order.
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let dir = |e: &Entry| e.kind != EntryKind::Dir;
        dir(a)
            .cmp(&dir(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// A hidden (dot) file or folder.
pub fn hidden(entry: &Entry) -> bool {
    entry.name.starts_with('.') && entry.name != UP
}

/// An entry the selection may hold: not `..`, and named in UTF-8 (an entry whose name is not
/// is never acted on, 01 §3a).
pub fn selectable(entry: &Entry) -> bool {
    entry.name != UP && !entry.lossy
}

/// Whether `name` matches the select-group `pattern` (04 §4): `*` any run of characters, `?`
/// one character, every other character itself in any case.
pub fn matches(pattern: &str, name: &str) -> bool {
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut i, mut j) = (0, 0);
    // The last `*` seen: where it is in the pattern, and how much of the name it has taken.
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if i < p.len() && (p[i] == '?' || same(p[i], n[j])) {
            i += 1;
            j += 1;
        } else if let Some((at, taken)) = star {
            // The `*` takes one more character; the rest of the pattern tries again after it.
            star = Some((at, taken + 1));
            i = at + 1;
            j = taken + 1;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

/// The Name and Ext columns: a file's extension apart (`main.rs` → `main`, `rs`); a folder,
/// a dot file (`.bashrc`, `.config.json`) and a name without a dot keep the whole name.
pub fn split_name(entry: &Entry) -> (&str, &str) {
    let name = entry.name.as_str();
    if entry.kind == EntryKind::Dir || name.starts_with('.') {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

/// The Size column: the size of a file, else what the entry is.
pub fn size_cell(entry: &Entry) -> String {
    match entry.kind {
        _ if entry.name == UP => "<UP>".to_string(),
        EntryKind::Dir if entry.link => "<LINK>".to_string(),
        EntryKind::Dir => "<DIR>".to_string(),
        EntryKind::File => grouped(u128::from(entry.size)),
        EntryKind::Special => "<SPECIAL>".to_string(),
        EntryKind::Broken => "<BROKEN>".to_string(),
        EntryKind::Unknown => "?".to_string(),
    }
}

/// The Size column's text in `cells` characters: [`size_cell`] when it fits, else the size in
/// the smallest unit (KB, MB, GB, TB, PB, EB — powers of 1000, rounded) that does — a size is
/// never cut.
pub fn size_in(entry: &Entry, cells: usize) -> String {
    let exact = size_cell(entry);
    if exact.chars().count() <= cells || entry.kind != EntryKind::File || entry.name == UP {
        return exact;
    }
    let mut div: u128 = 1;
    for unit in ["KB", "MB", "GB", "TB", "PB", "EB"] {
        div *= 1000;
        let value = (u128::from(entry.size) + div / 2) / div;
        let text = format!("{} {unit}", grouped(value));
        if text.chars().count() <= cells {
            return text;
        }
    }
    exact
}

/// The Date and Time columns, in local time (`2026-11-19`, `14:03`); blank for `..` and for an
/// entry whose attributes could not be read.
pub fn date_cells(entry: &Entry) -> (String, String) {
    if entry.name == UP || entry.kind == EntryKind::Unknown {
        return (String::new(), String::new());
    }
    let Some(tm) = crate::gui::chrome::local_tm(unix_secs(entry.mtime)) else {
        return (String::new(), String::new());
    };
    (
        format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        ),
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min),
    )
}

/// Seconds since 1970, floored: a moment before it is in the second that starts at or before
/// it (half a second before is -1).
pub fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => {
            let d = e.duration();
            -(d.as_secs() as i64) - i64::from(d.subsec_nanos() > 0)
        }
    }
}

/// A row as plain text, the columns the panel shows (the `SOPEN_LIST` hook prints these).
pub fn row_text(entry: &Entry) -> String {
    let (name, ext) = split_name(entry);
    let (date, time) = date_cells(entry);
    format!(
        "{name:<24} {ext:<5} {:>14} {date:<10} {time:<5}",
        size_cell(entry)
    )
    .trim_end()
    .to_string()
}

#[cfg(test)]
mod tests {
    //! The selection on the panel model (04 §4, K-S1): Space and Ins, `+ - *`, Cmd/Ctrl+A, the
    //! foot, a re-read and another folder. The keys themselves are S1-A's table; these are what
    //! they call.

    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_string(),
            lossy: false,
            kind,
            link: false,
            size,
            mtime: SystemTime::UNIX_EPOCH,
        }
    }

    fn file(name: &str, size: u64) -> Entry {
        entry(name, EntryKind::File, size)
    }

    fn dir(name: &str) -> Entry {
        entry(name, EntryKind::Dir, 0)
    }

    /// `/srv/src`: `..`, two folders, five files (one named in bad UTF-8), in that order.
    fn panel() -> Panel {
        let mut bad = file("bad\u{FFFD}.bin", 7);
        bad.lossy = true;
        let mut p = Panel::new("/srv/src");
        p.fill("/srv/src".to_string(), listing(bad), Some(1_000), None);
        p
    }

    fn listing(bad: Entry) -> Vec<Entry> {
        vec![
            file("main.rs", 100),
            dir("docs"),
            file("사진.png", 2_000),
            dir("한글 폴더"),
            file("lib.RS", 20),
            bad,
            file(".hidden", 3),
        ]
    }

    fn selected(p: &Panel) -> Vec<String> {
        p.selection().iter().map(|e| e.name.clone()).collect()
    }

    fn names(p: &Panel) -> Vec<&str> {
        p.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn the_fixture_lists_up_folders_then_files() {
        let p = panel();
        assert_eq!(
            names(&p),
            [
                "..",
                "docs",
                "한글 폴더",
                ".hidden",
                "bad\u{FFFD}.bin",
                "lib.RS",
                "main.rs",
                "사진.png"
            ]
        );
    }

    /// Space (and Ins: the same call) selects the bar's entry and steps down; again on it
    /// unselects; `..` and a name that is not UTF-8 are only stepped over; the last row stays.
    #[test]
    fn space_toggles_the_bar_and_steps_down() {
        let mut p = panel();
        p.toggle(); // `..`: never selected, the bar moves on
        assert_eq!((selected(&p), p.highlight), (Vec::<String>::new(), 1));
        p.toggle(); // docs
        p.toggle(); // 한글 폴더
        assert_eq!(selected(&p), ["docs", "한글 폴더"]);
        assert_eq!(p.highlight, 3);
        p.toggle(); // .hidden
        p.toggle(); // bad name: stepped over
        assert_eq!(selected(&p), ["docs", "한글 폴더", ".hidden"]);
        assert_eq!(p.highlight, 5);
        p.highlight = 2;
        p.toggle(); // 한글 폴더 again: unselected
        assert_eq!(selected(&p), ["docs", ".hidden"]);
        assert_eq!(p.highlight, 3);
        p.end();
        p.toggle();
        p.toggle();
        assert_eq!(
            selected(&p),
            ["docs", ".hidden"],
            "the last row twice: on, off"
        );
        assert_eq!(
            p.highlight,
            p.entries.len() - 1,
            "the bar stays on the last row"
        );
        assert!(!p.is_selected(&p.entries[0]));
    }

    /// Space ×3 (K-S1): three entries, the foot `3 selected = …` — the files' bytes, a folder
    /// counted as an entry — before the counts; with nothing selected the foot is as before.
    #[test]
    fn the_foot_says_what_is_selected_first() {
        let mut p = panel();
        let plain = p.footer();
        assert_eq!(plain, "7 Listed = 2,130 bytes · 1,000 Bytes Free");
        assert_eq!(p.selected_foot(), None);
        p.highlight = 1;
        p.toggle(); // docs
        p.highlight = 6;
        p.toggle(); // main.rs
        p.toggle(); // 사진.png
        assert_eq!(
            p.selected_foot().as_deref(),
            Some("3 selected = 2,100 bytes")
        );
        let (count, free) = p.foot_parts();
        assert_eq!(count, "3 selected = 2,100 bytes · 7 Listed = 2,130 bytes");
        assert_eq!(free.as_deref(), Some("1,000 Bytes Free"));
        assert_eq!(
            p.footer(),
            "3 selected = 2,100 bytes · 7 Listed = 2,130 bytes · 1,000 Bytes Free"
        );
        p.clear_selection();
        assert_eq!(p.footer(), plain);
    }

    /// `+` with a pattern selects the matches (any case, folders too), `-` unselects them; a
    /// bad UTF-8 name and `..` never match into the selection.
    #[test]
    fn plus_and_minus_select_and_unselect_a_group() {
        let mut p = panel();
        assert_eq!(p.select_group("*.rs", true), 2);
        assert_eq!(selected(&p), ["lib.RS", "main.rs"]);
        assert_eq!(
            p.select_group("*", true),
            6,
            "all but `..` and the bad name"
        );
        assert_eq!(
            selected(&p),
            [
                "docs",
                "한글 폴더",
                ".hidden",
                "lib.RS",
                "main.rs",
                "사진.png"
            ]
        );
        assert_eq!(p.select_group("한*", false), 1);
        assert_eq!(
            p.select_group("?????", false),
            1,
            "한글 폴더, unselected already"
        );
        assert_eq!(p.select_group("????", false), 1, "docs");
        assert_eq!(selected(&p), [".hidden", "lib.RS", "main.rs", "사진.png"]);
        assert_eq!(p.select_group("nothing*", true), 0);
    }

    /// `*` inverts files and folders alike; Cmd/Ctrl+A selects all — twice is still all.
    #[test]
    fn star_inverts_and_select_all_takes_everything() {
        let mut p = panel();
        p.select_group("*.rs", true);
        p.invert();
        assert_eq!(selected(&p), ["docs", "한글 폴더", ".hidden", "사진.png"]);
        p.invert();
        assert_eq!(selected(&p), ["lib.RS", "main.rs"]);
        p.select_all();
        p.select_all();
        assert_eq!(
            selected(&p),
            [
                "docs",
                "한글 폴더",
                ".hidden",
                "lib.RS",
                "main.rs",
                "사진.png"
            ]
        );
        p.invert();
        assert!(selected(&p).is_empty());
    }

    /// A re-read (Cmd/Ctrl+R, after an operation) keeps the names still there; another folder
    /// clears the selection, also when the panel comes back to the first one.
    #[test]
    fn a_reread_keeps_the_selection_another_folder_clears_it() {
        let mut p = panel();
        p.select_group("*", true);
        let mut fewer: Vec<Entry> = listing(file("x", 1))
            .into_iter()
            .filter(|e| e.name != "main.rs")
            .collect();
        fewer.push(file("new.txt", 5));
        p.fill("/srv/src".to_string(), fewer.clone(), None, Some("lib.RS"));
        assert_eq!(
            selected(&p),
            ["docs", "한글 폴더", ".hidden", "lib.RS", "사진.png"],
            "main.rs is gone, new.txt is not selected"
        );
        assert_eq!(p.current().unwrap().name, "lib.RS");
        p.fill("/srv/src/docs".to_string(), vec![file("a", 1)], None, None);
        assert!(selected(&p).is_empty(), "another folder");
        p.fill("/srv/src".to_string(), fewer, None, None);
        assert!(selected(&p).is_empty(), "back again: still cleared");
    }

    /// Copy, Move and Delete act on the selection, else on the bar's entry — never `..` or a
    /// bad name; an operation's done items can be unselected one by one.
    #[test]
    fn what_an_operation_acts_on() {
        let mut p = panel();
        let acted =
            |p: &Panel| -> Vec<String> { p.acted_on().iter().map(|e| e.name.clone()).collect() };
        assert!(acted(&p).is_empty(), "the bar on `..`");
        p.highlight = 4;
        assert!(acted(&p).is_empty(), "the bar on a bad name");
        p.highlight = 7;
        assert_eq!(acted(&p), ["사진.png"]);
        p.select_group("*.rs", true);
        assert_eq!(acted(&p), ["lib.RS", "main.rs"], "the selection wins");
        p.unselect("lib.RS");
        assert_eq!(acted(&p), ["main.rs"]);
    }

    #[test]
    fn patterns_match_any_case_with_star_and_question_mark() {
        for (pattern, name, want) in [
            ("*", "anything", true),
            ("*", ".hidden", true),
            ("*", "", true),
            ("*.rs", "MAIN.RS", true),
            ("*.RS", "main.rs", true),
            ("*.rs", "main.rsx", false),
            ("m?in.*", "main.rs", true),
            ("m?in.*", "mn.rs", false),
            ("?", "한", true),
            ("한*", "한글 폴더", true),
            ("*폴더", "한글 폴더", true),
            ("*a*b*c", "xaybzc", true),
            ("*a*b*c", "xaybzcd", false),
            ("a**", "a", true),
            ("ÄB*", "äbc", true),
            ("", "", true),
            ("", "x", false),
            ("main.rs", "main.rs", true),
            ("main.rs", "main_rs", false),
        ] {
            assert_eq!(matches(pattern, name), want, "{pattern:?} vs {name:?}");
        }
    }
}
