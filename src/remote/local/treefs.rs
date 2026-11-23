//! [`TreeFs`] on the local disk (05 §6.2, §6.3, §7): bulk listings (macOS `getattrlistbulk`,
//! Linux `getdents64` + `statx`), the mount table (`getmntinfo(3)`, `/proc/self/mountinfo`) and
//! `statvfs`, each call on the watchdog pool. S5-A writes it.
//!
//! S5-0 scaffold: the implementation, with no methods yet.

use super::LocalFs;
use crate::remote::treefs::TreeFs;

impl TreeFs for LocalFs {}
