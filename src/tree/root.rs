//! The root (05 §2): the folder resolved by `realpath` and the home once per host; the root is the
//! home when the folder is the home or inside it — locally by `(st_dev, st_ino)`, remotely by a
//! component-wise prefix of the resolved paths — otherwise `/` (`T-root`). S5-A writes it.
//!
//! S5-0 scaffold: the file and its contract; nothing in it yet.
