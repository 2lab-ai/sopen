//! One file panel: the folder it shows, its entries in the browser's order (`..` first, then
//! folders, then files, each by name), the highlight bar and the first row on screen — and how a
//! row and the footer read (`N Listed = N bytes`, then `N Bytes Free`, in PC Shell's words).

use crate::remote::{Entry, EntryKind};
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
        }
    }

    /// Shows `dir` with its `entries`, sorted, `..` first unless it is the root; the highlight
    /// on `select` (the folder just left, going up) or on the first row.
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

    /// `N Listed = N bytes` (the entries but `..`, the bytes of the files), then the free space
    /// as `N Bytes Free` — PC Shell's two window feet on one line.
    pub fn footer(&self) -> String {
        match self.foot_parts() {
            (listed, Some(free)) => format!("{listed} · {free}"),
            (listed, None) => listed,
        }
    }

    /// The footer's two parts: `N Listed = N bytes`, and `N Bytes Free` when the listing said.
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
        (count, free)
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
