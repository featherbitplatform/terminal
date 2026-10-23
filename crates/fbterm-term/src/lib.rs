//! Terminal emulation and local shells.

mod input;
mod kitty;
mod kitty_guard;
mod palette;
mod selection;
mod terminal;
mod widget;

pub use input::{
    KeyAction, KeyInput, MouseButton, MouseKind, MouseReport, encode_key, encode_key_input,
    encode_mouse, encode_paste,
};
pub use palette::{ColorScheme, blend, contrast_ratio};
pub use selection::Selection;
pub use terminal::{EventProxy, GridSize, SharedTerminal, TermEvent, TermMode, Terminal};
pub use widget::{
    TerminalOutput, TerminalStyle, cell_colors, grid_size_from_bounds, pixel_to_cell,
    pixel_to_point, terminal_view,
};
