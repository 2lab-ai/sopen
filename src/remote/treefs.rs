//! The directory tree's reads (05 §7): bulk listings with device, inode, flags and mount status,
//! the mount table, `statvfs` and `expand-path`. They are a trait of their own, implemented for
//! [`SftpRemote`](super::SftpRemote) in `sftp/treefs.rs` and for [`LocalFs`](super::LocalFs) in
//! `local/treefs.rs` — submodules, which see their parent's private fields — so `sftp.rs`,
//! `local.rs` and [`RemoteFs`](super::RemoteFs) stay as they are (02 §5). Nothing runs on the
//! remote: SFTP requests only (05 §6.3). S5-A writes them.
//!
//! S5-0 scaffold: the trait, with no methods yet.

/// The tree's reads of one host. It is a **worker with a request channel and a result channel**,
/// not a `&mut self` call per request (05 §7): remotely it owns the tree's own SFTP connection and
/// its runtime, keeps up to 16 requests in flight, streams each result as it arrives, reports
/// `not answering` after 2 s and drops a cancelled request's future; locally it is the watchdog
/// pool of 05 §6.2. S5-A adds the methods.
pub trait TreeFs {}
