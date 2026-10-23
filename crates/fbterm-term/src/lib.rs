//! Terminal emulation and local shells.

mod input;
mod selection;
mod terminal;

pub use input::{MouseButton, MouseKind, MouseReport, encode_key, encode_mouse, encode_paste};
pub use selection::Selection;
pub use terminal::{EventProxy, GridSize, SharedTerminal, TermEvent, TermMode, Terminal};
