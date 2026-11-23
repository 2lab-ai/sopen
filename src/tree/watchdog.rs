//! The watchdog (05 §6.2): every local filesystem call of the tree runs on a pool thread and is
//! abandoned, never awaited, after 2 s; at most 4 abandoned calls are outstanding in the whole
//! process (`T-watchdog`). S5-A writes it.
//!
//! S5-0 scaffold: the file and its contract; nothing in it yet.
