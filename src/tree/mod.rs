//! The directory tree's model (05): what F10 shows in place of the active panel's list — the root
//! rule, which folders are shown, listed, sized and opened by themselves, each row's info line,
//! the background scans and the watchdog every local call runs behind. Its reads go through
//! [`crate::remote::treefs::TreeFs`]; the browser draws it (`gui::browser::tree`, S5-B). S5-A
//! writes it.
//!
//! S5-0 scaffold: one module per part of 05 §7, each a file and its contract, nothing in it yet.

mod cache;
mod expand;
mod info;
mod root;
mod rules;
mod scan;
mod watchdog;
