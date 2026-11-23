//! [`TreeFs`] over SFTP (05 §6.4, §7): the tree's own connection, driven by a worker that clones
//! the cheap `Fs` handle and keeps up to 16 requests in flight (fewer when `limits@openssh.com`
//! says so) instead of one `block_on` per call; `expand-path@openssh.com`, `statvfs@openssh.com`
//! and `/proc/self/mountinfo` read over SFTP. S5-A writes it.
//!
//! S5-0 scaffold: the implementation, with no methods yet.

use super::SftpRemote;
use crate::remote::treefs::TreeFs;

impl TreeFs for SftpRemote {}
