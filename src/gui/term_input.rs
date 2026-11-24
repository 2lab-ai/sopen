//! The terminal's input as the window's (10 §4, §9): crossterm's events become `egui::Event`s in
//! egui-winit 0.37's shape — a named key with its modifiers, Ctrl as egui's `command` on every OS,
//! ⌘ only under the kitty protocol, a printable ASCII key as `Key` and `Text`, other text as `Text`
//! alone, mouse at the cell's centre, wheel by lines, focus as `WindowFocused` — so the `FKeyInput`
//! hook and the key table see them as from the window.
