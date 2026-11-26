//! What the plugin shows, made from its state: nothing here calls `docker`.

mod detail;
mod dialog;
mod panel;
mod registry;

pub use detail::{container, image, object};
pub use dialog::dialog;
pub use panel::{console, panel};
pub use registry::registry;

use ardoise_plugin::ui::*;
use rust_i18n::t;

/// A picto with its name as a tooltip.
pub fn picto(key: &str, action: &str, icon: &str) -> Node {
    button(t!(key), action)
        .button_style(ButtonStyle::Ghost)
        .icon(icon)
}

/// A button with a picto and its name.
pub fn labelled(key: &str, action: &str, icon: &str) -> Node {
    button(t!(key), action).icon(icon).with_label()
}

pub fn muted(value: impl Into<String>) -> Node {
    text(value).style(TextStyle::Muted)
}

pub fn heading(value: impl Into<String>) -> Node {
    text(value).style(TextStyle::Bold)
}

/// An error, when there is one.
pub fn error_line(error: &str) -> Option<Node> {
    (!error.is_empty()).then(|| text(error).style(TextStyle::Error))
}

/// A table to read: the titles, then the rows.
pub fn table(id: &str, titles: &[String], rows: &[Vec<String>]) -> Grid {
    grid(
        id,
        titles.iter().map(|name| GridColumn {
            name: name.clone(),
            ..GridColumn::default()
        }),
        rows.iter().map(|row| GridRow {
            cells: row
                .iter()
                .map(|cell| GridCell {
                    text: cell.clone(),
                    ..GridCell::default()
                })
                .collect(),
            ..GridRow::default()
        }),
    )
}
