//! [`FileOps`] over SFTP (04 §5.6): a file renamed by `hardlink@openssh.com` then `REMOVE`
//! (never `posix-rename@openssh.com`, which replaces), `MKDIR`, `RMDIR`, `Fs::symlink(target,
//! link)`, `copy-data` in 8 MiB chunks into a temp created `CREAT|EXCL`, published by a hard link.
//! S1-C writes it.
//!
//! S1-0 scaffold: the implementation, with no methods yet.

use super::SftpRemote;
use crate::remote::ops::FileOps;

impl FileOps for SftpRemote {}
