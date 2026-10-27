//! Terminal emulation and local shells.

mod export;
mod input;
mod kitty;
mod palette;
mod search;
mod selection;
mod terminal;
mod widget;

pub use export::{ExportError, ExportFormat, ExportSummary, export, write_file};
pub use input::{
    KeyAction, KeyInput, MouseButton, MouseKind, MouseReport, encode_key, encode_key_input,
    encode_mouse, encode_paste,
};
pub use palette::{ColorScheme, blend, contrast_ratio};
pub use search::{
    CHUNK_BUDGET, CHUNK_ROWS, Cover, Hit, MATCH_CAP, MAX_QUERY_CHARS, PatternError, SearchMatch,
    SearchOutcome, SearchPattern, SearchQuery, SearchRequest, Snippet, StablePoint,
    is_case_sensitive, search, snippet,
};
pub use selection::Selection;
pub use terminal::{EventProxy, GridSize, SharedTerminal, TermEvent, TermMode, Terminal};
pub use widget::{
    DrawList, Highlight, RIGHT_CLICK_MENU, TerminalOutput, TerminalStyle, build_draw_list,
    cell_colors, grid_size_from_bounds, highlighted_terminal_view, paste_input, pixel_to_cell,
    pixel_to_point, terminal_view,
};
