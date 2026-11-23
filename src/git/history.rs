//! The log stream (03 §4.5): one `git log` per load, its roots as object ids on stdin (§4.1),
//! read 500 records at a time from the same stream, parked while hidden and ended after 30 s
//! hidden or when its panel is dropped. S2-B writes it.
//!
//! S2-0 scaffold: the file, so S2-B edits only its own.
