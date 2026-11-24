//! The TUI's cell core (10 §4), shared by the window and the terminal: every screen is drawn into
//! a `ratatui_core::Buffer` — the grid (`Grid::over`, `rect`), the painter (background runs, a
//! glyph per cell, a wide glyph's second cell in its colours), the layer record, the cell API
//! (text, fill, frame, scroll bar, F-key row, dialog, field, button; colour only, no modifiers) and
//! the read-back. Widths by unicode-width (10 §10).
