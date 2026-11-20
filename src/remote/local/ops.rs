//! [`FileOps`] on the local disk (04 §5.6): `renameat2(RENAME_NOREPLACE)` on Linux and
//! `renamex_np(RENAME_EXCL)` on macOS (else `link` + `unlink`), `mkdir`, `rmdir`, links, a copy
//! into a temp created exclusively and published by `link`. S1-C writes it.
//!
//! S1-0 scaffold: the implementation, with no methods yet.

use super::LocalFs;
use crate::remote::ops::FileOps;

impl FileOps for LocalFs {}
