//! Exporting a terminal's text: the normal screen's whole history (scrollback and screen) as
//! plain text or with colors (SGR escape sequences), optionally gzip-compressed.
//!
//! [`Snapshot::take`] copies the grid while the terminal is locked (a clone of its rows, a
//! few milliseconds for a full scrollback); formatting, compressing and writing then run on
//! the copy with the lock released. Rows joined by a soft wrap become one line again; wide
//! characters are written once (their spacer cells skipped) and combining characters follow
//! the character they belong to.

use std::io::{self, Write};
use std::path::Path;

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::Terminal;

/// What an export file holds, chosen by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// `.txt` (and any other name).
    Plain,
    /// `.txt.gz` (and any other `.gz`).
    PlainGz,
    /// `.ansi`: colors and text attributes as SGR escape sequences.
    Ansi,
    /// `.ansi.gz`.
    AnsiGz,
}

impl ExportFormat {
    /// Every format, in the order a save dialog lists them.
    pub const ALL: [ExportFormat; 4] = [Self::Plain, Self::PlainGz, Self::Ansi, Self::AnsiGz];

    /// The format a file name asks for: `.ansi.gz`, `.ansi`, `.gz`, else plain text. Case
    /// does not matter.
    pub fn from_path(path: &Path) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.ends_with(".ansi.gz") {
            Self::AnsiGz
        } else if name.ends_with(".gz") {
            Self::PlainGz
        } else if name.ends_with(".ansi") {
            Self::Ansi
        } else {
            Self::Plain
        }
    }

    /// The file-name extension, without the leading dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Plain => "txt",
            Self::PlainGz => "txt.gz",
            Self::Ansi => "ansi",
            Self::AnsiGz => "ansi.gz",
        }
    }

    /// The name a save dialog shows for it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "Plain text",
            Self::PlainGz => "Plain text, gzip",
            Self::Ansi => "With colors",
            Self::AnsiGz => "With colors, gzip",
        }
    }

    pub fn colors(self) -> bool {
        matches!(self, Self::Ansi | Self::AnsiGz)
    }

    pub fn gzip(self) -> bool {
        matches!(self, Self::PlainGz | Self::AnsiGz)
    }
}

/// A copy of a terminal's normal screen and scrollback.
pub struct Snapshot {
    grid: Grid<Cell>,
    /// The alternate screen (vim, less, top) was up: the copy is of the normal screen behind
    /// it, not of what was showing.
    pub alt_screen: bool,
}

impl Snapshot {
    /// Copy `term`'s normal screen and its history.
    pub fn take(term: &Terminal) -> Self {
        let alt_screen = term.mode().contains(crate::TermMode::ALT_SCREEN);
        let grid = if alt_screen {
            term.term.inactive_grid()
        } else {
            term.term.grid()
        };
        Self {
            grid: grid.clone(),
            alt_screen,
        }
    }

    /// The text in `format` (gzip aside).
    pub fn render(&self, format: ExportFormat) -> String {
        if format.colors() {
            ansi(&self.grid)
        } else {
            plain(&self.grid)
        }
    }

    /// Lines of history and screen.
    pub fn rows(&self) -> usize {
        self.grid.total_lines()
    }
}

/// Write `text` to `path` in `format` (gzip-compressed for the `.gz` formats).
pub fn write_file(path: &Path, format: ExportFormat, text: &str) -> io::Result<()> {
    let file = io::BufWriter::new(std::fs::File::create(path)?);
    if format.gzip() {
        let mut gz = GzEncoder::new(file, Compression::default());
        gz.write_all(text.as_bytes())?;
        gz.finish()?.flush()
    } else {
        let mut file = file;
        file.write_all(text.as_bytes())?;
        file.flush()
    }
}

/// The cells of each logical line, oldest first: rows continued by a soft wrap joined, and
/// the empty lines at the very end left out.
fn logical_lines(grid: &Grid<Cell>) -> Vec<Vec<&Cell>> {
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32;
    let mut lines = Vec::new();
    let mut current: Vec<&Cell> = Vec::new();
    for l in top..bottom {
        let row = &grid[Line(l)];
        for c in 0..cols {
            current.push(&row[Column(c)]);
        }
        let wraps = cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        if !wraps || l == bottom - 1 {
            lines.push(std::mem::take(&mut current));
        }
    }
    while lines
        .last()
        .is_some_and(|line| line.iter().all(|c| blank(c) && plain_style(c)))
    {
        lines.pop();
    }
    lines
}

/// A cell that only holds the right half of a wide character, or marks that one did not fit
/// at the end of the row.
fn spacer(cell: &Cell) -> bool {
    cell.flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
}

fn blank(cell: &Cell) -> bool {
    spacer(cell) || cell.flags.contains(Flags::HIDDEN) || cell.c == ' ' || cell.c == '\0'
}

/// A blank cell with this style looks like no cell at all (so it can be trimmed).
fn plain_style(cell: &Cell) -> bool {
    !cell
        .flags
        .intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
        && default_bg(cell.bg)
}

fn default_bg(c: Color) -> bool {
    sgr_color(c, false).is_none()
}

/// The text a cell adds: its character and combining characters; nothing for a spacer;
/// hidden text as spaces.
fn push_cell(out: &mut String, cell: &Cell) {
    if spacer(cell) {
        return;
    }
    if cell.flags.contains(Flags::HIDDEN) {
        out.push(' ');
        if cell.flags.contains(Flags::WIDE_CHAR) {
            out.push(' ');
        }
        return;
    }
    out.push(if cell.c == '\0' { ' ' } else { cell.c });
    if let Some(zw) = cell.zerowidth() {
        out.extend(zw);
    }
}

/// Plain UTF-8 text, one logical line per line, trailing spaces trimmed.
pub fn plain(grid: &Grid<Cell>) -> String {
    let mut out = String::new();
    for line in logical_lines(grid) {
        let start = out.len();
        for cell in line {
            push_cell(&mut out, cell);
        }
        let end = start + out[start..].trim_end_matches(' ').len();
        out.truncate(end);
        out.push('\n');
    }
    out
}

/// The SGR parameters for a color, `None` for the default one.
fn sgr_color(c: Color, fg: bool) -> Option<String> {
    let (base, bright, extended) = if fg { (30, 90, 38) } else { (40, 100, 48) };
    match c {
        Color::Named(n) => {
            let i = n as usize;
            let dim = NamedColor::DimBlack as usize;
            match i {
                0..=7 => Some((base + i).to_string()),
                8..=15 => Some((bright + i - 8).to_string()),
                _ if (dim..dim + 8).contains(&i) => Some((base + i - dim).to_string()),
                _ => None,
            }
        }
        Color::Indexed(n) => Some(format!("{extended};5;{n}")),
        Color::Spec(rgb) => Some(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

/// The SGR sequence that sets a cell's style from a reset, or `None` for the default style.
fn sgr(cell: &Cell) -> Option<String> {
    let mut params: Vec<String> = Vec::new();
    let f = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if f.contains(flag) {
            params.push(code.into());
        }
    }
    if f.intersects(Flags::ALL_UNDERLINES) {
        params.push(
            if f.contains(Flags::DOUBLE_UNDERLINE) {
                "21"
            } else {
                "4"
            }
            .into(),
        );
    }
    params.extend(sgr_color(cell.fg, true));
    params.extend(sgr_color(cell.bg, false));
    (!params.is_empty()).then(|| format!("\x1b[0;{}m", params.join(";")))
}

/// Text with colors and attributes as SGR sequences that `less -R` and `cat` replay: a style
/// change starts from a reset, every line ends in the default style, and trailing blanks
/// that would look like nothing are trimmed.
pub fn ansi(grid: &Grid<Cell>) -> String {
    let mut out = String::new();
    for line in logical_lines(grid) {
        let mut cells: Vec<&Cell> = line.into_iter().filter(|c| !spacer(c)).collect();
        while cells.last().is_some_and(|c| blank(c) && plain_style(c)) {
            cells.pop();
        }
        let mut style: Option<String> = None;
        for cell in cells {
            let next = sgr(cell);
            if next != style {
                out.push_str(next.as_deref().unwrap_or("\x1b[0m"));
                style = next;
            }
            push_cell(&mut out, cell);
        }
        if style.is_some() {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GridSize;

    fn term(cols: u16, rows: u16, bytes: &[u8]) -> Terminal {
        let mut t = Terminal::new(
            GridSize {
                cols,
                rows,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            1000,
        );
        t.feed(bytes);
        t
    }

    fn gunzip(bytes: &[u8]) -> io::Result<String> {
        use std::io::Read;
        let mut s = String::new();
        flate2::read::GzDecoder::new(bytes).read_to_string(&mut s)?;
        Ok(s)
    }

    fn text(t: &Terminal, format: ExportFormat) -> String {
        Snapshot::take(t).render(format)
    }

    #[test]
    fn plain_joins_wrapped_rows_and_trims_trailing_spaces() {
        // 10 columns: the first line wraps onto a second row.
        let t = term(10, 5, b"abcdefghijKLM  \r\nshort   \r\n\r\nend");
        assert_eq!(
            text(&t, ExportFormat::Plain),
            "abcdefghijKLM\nshort\n\nend\n"
        );
    }

    #[test]
    fn spaces_at_a_soft_wrap_are_kept() {
        let t = term(5, 4, b"abc  def");
        assert_eq!(text(&t, ExportFormat::Plain), "abc  def\n");
    }

    #[test]
    fn history_comes_first_in_order() {
        let mut bytes = Vec::new();
        for i in 0..50 {
            bytes.extend(format!("line {i}\r\n").bytes());
        }
        let t = term(20, 5, &bytes);
        let out = text(&t, ExportFormat::Plain);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[49], "line 49");
    }

    #[test]
    fn wide_and_combining_characters_are_written_once_and_whole() {
        // "日本" is two wide characters; "e" + U+0301 combines.
        let t = term(10, 3, "日本 e\u{301}x".as_bytes());
        assert_eq!(text(&t, ExportFormat::Plain), "日本 e\u{301}x\n");
        // A wide character that does not fit at the end of a row moves to the next one.
        let t = term(5, 3, "abcd日".as_bytes());
        assert_eq!(text(&t, ExportFormat::Plain), "abcd日\n");
    }

    #[test]
    fn ansi_keeps_colors_and_attributes_and_resets_at_line_ends() {
        let t = term(
            40,
            4,
            b"\x1b[1;31mred\x1b[0m plain \x1b[38;5;208mo\x1b[48;2;1;2;3mx\x1b[0m\r\n\x1b[3;4;7;9;2mstyled\x1b[0m   ",
        );
        let out = text(&t, ExportFormat::Ansi);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "\x1b[0;1;31mred\x1b[0m plain \x1b[0;38;5;208mo\x1b[0;38;5;208;48;2;1;2;3mx\x1b[0m"
        );
        assert_eq!(lines[1], "\x1b[0;2;3;7;9;4mstyled\x1b[0m");
        // Bright colors use their own codes.
        let t = term(20, 2, b"\x1b[92mg\x1b[103my\x1b[0m");
        assert_eq!(
            text(&t, ExportFormat::Ansi),
            "\x1b[0;92mg\x1b[0;92;103my\x1b[0m\n"
        );
    }

    #[test]
    fn ansi_keeps_trailing_blanks_that_show() {
        // A colored background at the end of a line is visible, so it stays.
        let t = term(20, 2, b"a\x1b[44m  \x1b[0m   ");
        assert_eq!(text(&t, ExportFormat::Ansi), "a\x1b[0;44m  \x1b[0m\n");
        assert_eq!(text(&t, ExportFormat::Plain), "a\n");
    }

    #[test]
    fn the_alt_screen_exports_the_normal_screens_history() {
        let t = term(20, 4, b"normal 1\r\nnormal 2\r\n\x1b[?1049hFULL SCREEN APP");
        let snap = Snapshot::take(&t);
        assert!(snap.alt_screen);
        assert_eq!(snap.render(ExportFormat::Plain), "normal 1\nnormal 2\n");
        let t = term(20, 4, b"normal 1\r\n\x1b[?1049hAPP\x1b[?1049l");
        let snap = Snapshot::take(&t);
        assert!(!snap.alt_screen);
        assert_eq!(snap.render(ExportFormat::Plain), "normal 1\n");
    }

    #[test]
    fn the_format_follows_the_file_name() {
        for (name, f) in [
            ("a.txt", ExportFormat::Plain),
            ("a", ExportFormat::Plain),
            ("a.log", ExportFormat::Plain),
            ("a.TXT.GZ", ExportFormat::PlainGz),
            ("a.gz", ExportFormat::PlainGz),
            ("a.ansi", ExportFormat::Ansi),
            ("a.ansi.gz", ExportFormat::AnsiGz),
        ] {
            assert_eq!(ExportFormat::from_path(Path::new(name)), f, "{name}");
        }
        for f in ExportFormat::ALL {
            let name = format!("x.{}", f.extension());
            assert_eq!(ExportFormat::from_path(Path::new(&name)), f);
        }
    }

    #[test]
    fn gzip_files_round_trip() {
        let dir = std::env::temp_dir().join(format!("fbterm-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = term(20, 3, "héllo\r\n\x1b[31mworld\x1b[0m".as_bytes());
        for f in ExportFormat::ALL {
            let path = dir.join(format!("out.{}", f.extension()));
            let text = text(&t, f);
            write_file(&path, f, &text).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let back = if f.gzip() {
                assert_eq!(&bytes[..2], [0x1f, 0x8b], "gzip magic");
                gunzip(&bytes).unwrap()
            } else {
                String::from_utf8(bytes).unwrap()
            };
            assert_eq!(back, text);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_terminal_exports_nothing() {
        let t = term(20, 3, b"");
        assert_eq!(text(&t, ExportFormat::Plain), "");
        assert_eq!(text(&t, ExportFormat::Ansi), "");
    }
}
