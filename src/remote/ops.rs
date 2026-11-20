//! The primitives the browser's rename, copy and move are built from (04 §5.6): renames that never
//! replace, `MKDIR`, `RMDIR`, links read and made, a same-host `copy-data` in chunks, staged
//! temps. They are a trait of their own, implemented for [`SftpRemote`](super::SftpRemote) in
//! `sftp/ops.rs` and for [`LocalFs`](super::LocalFs) in `local/ops.rs` — submodules, which see
//! their parent's private fields — so [`RemoteFs`] and its write algorithm stay as they are (02
//! §5). Nothing runs on the remote: no `cp`, `mv` or `rm` over ssh. S1-C writes them; S6-A adds
//! the plain rename.
//!
//! S1-0 scaffold: the trait, with no methods yet.

use super::RemoteFs;

/// The file-operation primitives of one host. [`RemoteFs`] is its supertrait, so one connection
/// (a `Box<dyn FileOps + Send>`, an operation's own, 04 §5.1) serves both the stats and transfers
/// of `RemoteFs` and these. S1-C adds the methods; none of them replaces an existing name.
pub trait FileOps: RemoteFs {}
