//! The preview host (02 §4): in preview mode, the viewer of the file under the active list's bar,
//! drawn in the opposite side after the bar has rested 150 ms — told it is a preview
//! (`Window::set_preview(true)`), drawn disabled with the frame's events withheld, prepared one at
//! a time on the window's one preview connection, and handed over in place to a full viewer by
//! Enter or F3 (`set_preview(false)`). Every exit of 02 §4.4's table is defined here. S1-E writes
//! it (S1-M its media row).
//!
//! S1-0 scaffold: the file and its contract; nothing in it yet.
