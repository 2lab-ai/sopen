//! The info line (05 §6.1): each row's subfolders, files and size — a lower bound marked `+`
//! until the scan has counted everything below — or its tag (`<LINK>`, `<SYS>`, `<NET>`, `<AUTO>`,
//! `<SYSVOL>`, `?`, `not answering`, `=… fs`) (`T-info`).
//!
//! Sizes are in the list's units (powers of 1000, as the panel's Size column), the smallest unit
//! that fits in three digits, never cut; a fraction is shown below 10 and dropped, never rounded
//! up — a lower bound must not read more than was counted.

/// A count from a folder's own listing; `more`: the listing stopped early, so it is a lower
/// bound (`9 d+`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count {
    pub n: u64,
    pub more: bool,
}

/// What stands in the size column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Nothing read yet.
    Unknown,
    /// Bytes counted: the folder's own files and, as the scan reaches them, those below.
    Bytes {
        bytes: u64,
        /// More may be below (a lower bound): `+`.
        more: bool,
        /// From a scan of an earlier tree in this process, not run again since: `~`.
        cached: bool,
        /// A scan is counting it now: the moving mark after it.
        scanning: bool,
    },
    /// The filesystem mounted there (R3): `=87G fs`.
    Fs {
        used_bytes: u64,
    },
    Tag(Tag),
}

/// A tag in place of the size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// R1.
    Link,
    /// R4: a kernel or virtual filesystem.
    Sys,
    /// R5: a network or FUSE-remote filesystem.
    Net,
    /// R6: an automounter trigger.
    Auto,
    /// R7: a macOS `nobrowse` volume.
    SysVol,
    /// R2: unreadable.
    Unreadable,
    /// 05 §6.2.
    NotAnswering,
}

impl Tag {
    pub fn text(self) -> &'static str {
        match self {
            Tag::Link => "<LINK>",
            Tag::Sys => "<SYS>",
            Tag::Net => "<NET>",
            Tag::Auto => "<AUTO>",
            Tag::SysVol => "<SYSVOL>",
            Tag::Unreadable => "?",
            Tag::NotAnswering => "not answering",
        }
    }
}

/// A row's info line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    /// Subfolders, links to folders included; `None`: not listed.
    pub dirs: Option<Count>,
    /// Regular files and links to files (special files are not counted).
    pub files: Option<Count>,
    pub size: Size,
    /// The used file count of the filesystem mounted there (R3, R7), for the status row while
    /// the bar is on the row, once its `statvfs` has answered; `Some(None)`: a filesystem that
    /// counts no files (btrfs).
    pub fs_files: Option<Option<u64>>,
}

impl Default for Info {
    fn default() -> Self {
        Self {
            dirs: None,
            files: None,
            size: Size::Unknown,
            fs_files: None,
        }
    }
}

/// The moving mark after a size being counted (05 §6.3); the UI animates it.
pub const SCANNING_MARK: &str = "·";

impl Info {
    /// The three right-aligned columns as text: `19 d`, `40 f`, `9.8M+` (empty where nothing is
    /// known). A size being counted ends with ` ·`.
    pub fn columns(&self) -> [String; 3] {
        let count = |c: Option<Count>, unit: &str| match c {
            Some(c) => format!("{} {unit}{}", c.n, if c.more { "+" } else { "" }),
            None => String::new(),
        };
        let size = match self.size {
            Size::Unknown => String::new(),
            Size::Bytes {
                bytes,
                more,
                cached,
                scanning,
            } => format!(
                "{}{}{}{}",
                if cached { "~" } else { "" },
                human(bytes),
                if more { "+" } else { "" },
                if scanning {
                    format!(" {SCANNING_MARK}")
                } else {
                    String::new()
                }
            ),
            Size::Fs { used_bytes } => format!("={} fs", human(used_bytes)),
            Size::Tag(tag) => tag.text().to_string(),
        };
        [count(self.dirs, "d"), count(self.files, "f"), size]
    }

    /// The line as one string, the columns joined by spaces (tests, logs).
    pub fn text(&self) -> String {
        self.columns()
            .iter()
            .filter(|c| !c.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// `bytes` in the list's units: `60B`, `79K`, `9.8M`, `87G`, … (powers of 1000; below 10 with
/// one decimal; never rounded up).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes < 1000 {
        return format!("{bytes}B");
    }
    let mut div: u128 = 1;
    for unit in UNITS {
        div *= 1000;
        let whole = u128::from(bytes) / div;
        if whole < 10 {
            let tenth = u128::from(bytes) * 10 / div % 10;
            return format!("{whole}.{tenth}{unit}");
        }
        if whole < 1000 {
            return format!("{whole}{unit}");
        }
    }
    format!("{}E", u128::from(bytes) / div)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_in_the_lists_units_never_rounded_up() {
        assert_eq!(human(0), "0B");
        assert_eq!(human(60), "60B");
        assert_eq!(human(999), "999B");
        assert_eq!(human(1000), "1.0K");
        assert_eq!(human(79_999), "79K");
        assert_eq!(human(9_899_999), "9.8M");
        assert_eq!(human(87_000_000_000), "87G");
        assert_eq!(human(999_999_999), "999M");
        assert_eq!(human(u64::MAX), "18E");
    }

    #[test]
    fn the_columns_say_lower_bounds_cache_scans_and_tags() {
        let info = Info {
            dirs: Some(Count { n: 9, more: true }),
            files: Some(Count {
                n: 9991,
                more: true,
            }),
            size: Size::Bytes {
                bytes: 1_200_000,
                more: true,
                cached: false,
                scanning: false,
            },
            fs_files: None,
        };
        assert_eq!(info.text(), "9 d+ 9991 f+ 1.2M+");
        let scanning = Info {
            size: Size::Bytes {
                bytes: 9_800_000,
                more: true,
                cached: false,
                scanning: true,
            },
            ..info
        };
        assert_eq!(scanning.columns()[2], "9.8M+ ·");
        let cached = Info {
            size: Size::Bytes {
                bytes: 9_800_000,
                more: false,
                cached: true,
                scanning: false,
            },
            ..info
        };
        assert_eq!(cached.columns()[2], "~9.8M");
        let fs = Info {
            size: Size::Fs {
                used_bytes: 87_000_000_000,
            },
            ..info
        };
        assert_eq!(fs.columns()[2], "=87G fs");
        let link = Info {
            dirs: None,
            files: None,
            size: Size::Tag(Tag::Link),
            fs_files: None,
        };
        assert_eq!(link.text(), "<LINK>");
    }
}
