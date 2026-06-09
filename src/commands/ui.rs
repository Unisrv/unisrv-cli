//! Shared rendering helpers for the table-style command output (`host`,
//! `instance ls`, …) so colour handling and relative-time formatting live in one
//! place rather than being copy-pasted per command.

use chrono::NaiveDateTime;
use chrono_humanize::HumanTime;
use comfy_table::{Cell, Color};

/// Whether stdout currently supports ANSI colour. Centralised so every table
/// decides colour the same way.
pub fn colors_enabled() -> bool {
    console::Term::stdout().features().colors_supported()
}

/// Build a table cell, applying `color` only when colour is enabled.
pub fn cell_with_color(text: String, color: Option<Color>, use_color: bool) -> Cell {
    let cell = Cell::new(text);
    match (color, use_color) {
        (Some(c), true) => cell.fg(c),
        _ => cell,
    }
}

/// Render `when` relative to `now`, e.g. "5 minutes ago".
pub fn format_relative(when: NaiveDateTime, now: NaiveDateTime) -> String {
    HumanTime::from(when - now).to_string()
}
