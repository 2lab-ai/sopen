//! The lane layout (03 §4.3): `layout`, pure and deterministic — rows in display order in, a
//! `RowGraph` per row out (its column, its node kind and the segments that touch its node), the
//! slot intervals that painting derives pass-through lines from. S2-C writes it.
//!
//! S2-0 scaffold: the file, so S2-C edits only its own.
