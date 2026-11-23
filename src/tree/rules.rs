//! The rules R1–R10 and heuristics H1–H3 (05 §4, §4.1): for every folder row whether it is
//! listed unopened and sized, from the mount table, the `statvfs` fences and the folder's own
//! listing — never from `st_dev` alone (`T-norules`). None of them is ever opened by 05 §5.

use crate::remote::treefs::{Mount, Mounts};
use std::collections::HashMap;

/// What is mounted at a folder (R3–R7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// R3: a disk filesystem — listed and sized, its own figure shown at once (`=87G fs`).
    Disk,
    /// R4: a kernel or virtual filesystem.
    Sys,
    /// R5: a network or FUSE-remote filesystem.
    Net,
    /// R6: an automounter trigger.
    Auto,
    /// R7: a macOS `nobrowse` volume.
    SysVol,
}

/// Why a folder is never opened by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// R1.
    Link,
    /// R2.
    Unreadable,
    /// R3–R7.
    Mount(Class),
    /// R8: `CACHEDIR.TAG`.
    CacheDir,
    /// R9: version-control internals.
    Vcs,
    /// R10: a Python environment.
    Venv,
    /// H1.
    Hidden,
    /// H2: a generated folder's name.
    Generated,
    /// H3: macOS `~/Library` and `~/.Trash`.
    Private,
}

impl Rule {
    /// Read for its info line and children without the owner opening it.
    pub fn listed_unopened(self) -> bool {
        !matches!(
            self,
            Rule::Link
                | Rule::Private
                | Rule::Mount(Class::Sys | Class::Net | Class::Auto | Class::SysVol)
        )
    }

    /// Sized by the background scan (H3 only on Space).
    pub fn sized(self) -> bool {
        !matches!(
            self,
            Rule::Link
                | Rule::Unreadable
                | Rule::Private
                | Rule::Mount(Class::Sys | Class::Net | Class::Auto | Class::SysVol)
        )
    }

    /// Sized even on Space: never for R1, R2, R4–R7.
    pub fn sizable_on_space(self) -> bool {
        self.sized() || self == Rule::Private
    }
}

/// R4's filesystem types.
const SYS: [&str; 21] = [
    "proc",
    "sysfs",
    "devtmpfs",
    "devfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "securityfs",
    "debugfs",
    "tracefs",
    "bpf",
    "pstore",
    "efivarfs",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "binfmt_misc",
    "nsfs",
    "selinuxfs",
    "fuse.portal",
];

/// R5's filesystem types (and every other `fuse.*`).
const NET: [&str; 11] = [
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "afpfs",
    "webdav",
    "9p",
    "ceph",
    "glusterfs",
    "fuse.sshfs",
];

/// H2's names.
pub const GENERATED: [&str; 7] = [
    "node_modules",
    "__pycache__",
    "DerivedData",
    "Pods",
    ".gradle",
    ".next",
    ".tox",
];

/// The class of a mount by its type and flags: R6, R4, R5, R7, else R3.
pub fn class_of(fstype: &str, nobrowse: bool) -> Class {
    if fstype == "autofs" {
        Class::Auto
    } else if SYS.contains(&fstype) {
        Class::Sys
    } else if NET.contains(&fstype) || fstype.starts_with("fuse.") {
        Class::Net
    } else if nobrowse {
        Class::SysVol
    } else {
        Class::Disk
    }
}

/// What the tree knows of the host's mounts.
#[derive(Debug, Default)]
pub struct MountMap {
    /// Mount point → class, from the mount table.
    table: Option<HashMap<String, Class>>,
    /// A macOS host: H3 applies; remotely without a table, R6/R7 go by name.
    pub macos: bool,
    /// A remote without a table that offers `statvfs`: 05 §4.1's `f_fsid` fences.
    pub fences: bool,
}

impl MountMap {
    pub fn new(mounts: &Mounts, remote: bool) -> Self {
        let table = mounts.table.as_ref().map(|t: &Vec<Mount>| {
            t.iter()
                .map(|m| (m.at.clone(), class_of(&m.fstype, m.nobrowse)))
                .collect()
        });
        Self {
            fences: remote && table.is_none() && mounts.statvfs,
            table,
            macos: mounts.macos,
        }
    }

    /// The class of what is mounted at `path` by the table or, on a remote without one, by
    /// name (05 §4, §4.1); `None`: an ordinary folder as far as these know.
    pub fn class(&self, path: &str) -> Option<Class> {
        if let Some(table) = &self.table {
            return table.get(path).copied();
        }
        let mac = |names: &[&str]| self.macos && names.contains(&path);
        if ["/dev", "/proc"].contains(&path) {
            Some(Class::Sys)
        } else if mac(&["/home", "/net", "/Network", "/System/Volumes/Data/home"]) {
            Some(Class::Auto)
        } else if self.macos
            && path
                .strip_prefix("/System/Volumes/")
                .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
        {
            Some(Class::SysVol)
        } else {
            None
        }
    }

    /// The mount points at or below `path`, which a scan of it never enters.
    pub fn below(&self, path: &str) -> Vec<String> {
        match &self.table {
            Some(t) => t
                .keys()
                .filter(|at| at.as_str() != path && super::root::inside(at, path))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }
}

/// R9 by name.
pub fn vcs_name(name: &str) -> bool {
    name == ".hg" || name == ".svn"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_types_fall_into_their_rules() {
        assert_eq!(class_of("autofs", true), Class::Auto);
        assert_eq!(class_of("proc", false), Class::Sys);
        assert_eq!(class_of("devfs", true), Class::Sys);
        assert_eq!(class_of("fuse.portal", false), Class::Sys);
        assert_eq!(class_of("nfs4", false), Class::Net);
        assert_eq!(class_of("fuse.rclone", false), Class::Net);
        assert_eq!(class_of("fuse.something", false), Class::Net);
        assert_eq!(class_of("apfs", true), Class::SysVol);
        assert_eq!(class_of("apfs", false), Class::Disk);
        assert_eq!(class_of("btrfs", false), Class::Disk);
        assert_eq!(class_of("tmpfs", false), Class::Disk);
    }

    #[test]
    fn a_macos_remote_without_a_table_goes_by_name() {
        let mac = MountMap::new(
            &Mounts {
                table: None,
                macos: true,
                statvfs: true,
            },
            true,
        );
        assert!(mac.fences);
        assert_eq!(mac.class("/dev"), Some(Class::Sys));
        assert_eq!(mac.class("/home"), Some(Class::Auto));
        assert_eq!(mac.class("/System/Volumes/Data/home"), Some(Class::Auto));
        assert_eq!(mac.class("/System/Volumes/VM"), Some(Class::SysVol));
        assert_eq!(mac.class("/System/Volumes"), None);
        assert_eq!(mac.class("/Users"), None);
        let other = MountMap::new(
            &Mounts {
                table: None,
                macos: false,
                statvfs: false,
            },
            true,
        );
        assert!(!other.fences);
        assert_eq!(other.class("/proc"), Some(Class::Sys));
        assert_eq!(other.class("/home"), None);
    }
}
