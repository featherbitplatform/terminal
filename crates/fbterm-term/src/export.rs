//! Exporting a terminal's text: the normal screen's whole history (scrollback and screen) as
//! plain text or with colors (SGR escape sequences), optionally gzip-compressed.
//!
//! The history is read in chunks of [`CHUNK_ROWS`] rows with the terminal locked one chunk at
//! a time, each chunk formatted while locked and written out after the lock is released, so
//! output keeps flowing and no copy of the scrollback is ever made. Rows are addressed by
//! stable line number (the grid's scroll counter), so output arriving between chunks neither
//! repeats nor skips a line; a resize (which reflows the rows) ends the export with
//! [`ExportError::Resized`]. Rows joined by a soft wrap become one line again; wide
//! characters are written once (their spacer cells skipped) and combining characters follow
//! the character they belong to.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

use feathercritty::grid::{Dimensions, Grid, Row, RowReader};
use feathercritty::index::{Column, Line};
use feathercritty::term::cell::{Cell, Flags};
use feathercritty::vte::ansi::{Color, NamedColor};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::{SharedTerminal, TermMode, Terminal};

/// Rows formatted per lock of the terminal.
pub const CHUNK_ROWS: i64 = 1000;

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

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportSummary {
    /// The alternate screen (vim, less, top) was up when the export started: the file holds
    /// the normal screen behind it, not what was showing.
    pub alt_screen: bool,
}

#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    /// The terminal was resized while exporting (its rows reflowed).
    Resized,
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Resized => f.write_str("the tab was resized while exporting; try again"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The normal screen's grid: the shown one, or the one behind the alternate screen.
fn normal_grid(t: &Terminal) -> &Grid<Cell> {
    if t.mode().contains(TermMode::ALT_SCREEN) {
        t.term.inactive_grid()
    } else {
        t.term.grid()
    }
}

/// Write `terminal`'s history as `format` (gzip aside) to `out`.
pub fn export(
    terminal: &SharedTerminal,
    format: ExportFormat,
    out: &mut dyn Write,
) -> Result<ExportSummary, ExportError> {
    export_chunked(terminal, format, out, CHUNK_ROWS, &mut || {})
}

/// [`export`] with the chunk size and a hook run between chunks (with the lock released).
fn export_chunked(
    terminal: &SharedTerminal,
    format: ExportFormat,
    out: &mut dyn Write,
    chunk: i64,
    between: &mut dyn FnMut(),
) -> Result<ExportSummary, ExportError> {
    let (alt_screen, cols, mut next, end) = {
        let t = terminal.lock();
        let g = normal_grid(&t);
        let scrolled = g.scrolled_lines();
        (
            t.mode().contains(TermMode::ALT_SCREEN),
            g.columns(),
            scrolled - g.history_size() as i64,
            scrolled + g.screen_lines() as i64,
        )
    };
    let mut lines = LineWriter::new(format.colors());
    let mut reader = RowReader::new();
    let mut buf = String::new();
    while next < end {
        {
            let t = terminal.lock();
            let g = normal_grid(&t);
            if g.columns() != cols {
                return Err(ExportError::Resized);
            }
            let scrolled = g.scrolled_lines();
            let (top, bottom) = (-(g.history_size() as i64), g.screen_lines() as i64);
            let stop = (next + chunk.max(1)).min(end);
            for stable in next..stop {
                let line = stable - scrolled;
                // Rows that left the scrollback meanwhile are gone.
                if (top..bottom).contains(&line) {
                    let line = Line(line as i32);
                    lines.row(reader.row(g, line), g.wraps(line), cols, &mut buf);
                }
            }
            next = stop;
        }
        out.write_all(buf.as_bytes())?;
        buf.clear();
        if next < end {
            between();
        }
    }
    lines.finish(&mut buf);
    out.write_all(buf.as_bytes())?;
    Ok(ExportSummary { alt_screen })
}

/// Export `terminal` to `path` as `format`, gzip-compressed for the `.gz` formats. The file
/// is written next to `path` under a temporary name and renamed over it at the end, so an
/// existing file is replaced only by a complete export.
pub fn write_file(
    path: &Path,
    format: ExportFormat,
    terminal: &SharedTerminal,
) -> Result<ExportSummary, ExportError> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map_or_else(|| "export".into(), |n| n.to_string_lossy());
    let tmp = dir.join(format!(".{name}.{}.part", std::process::id()));
    let result = (|| {
        let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut writer = BufWriter::new(file);
        let summary = if format.gzip() {
            let mut gz = GzEncoder::new(writer, Compression::default());
            let summary = export(terminal, format, &mut gz)?;
            writer = gz.finish()?;
            summary
        } else {
            export(terminal, format, &mut writer)?
        };
        writer.flush()?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(summary)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Turns rows into lines of text: soft-wrapped rows joined, trailing blanks trimmed (with
/// colors: only blanks that look like nothing), empty lines at the very end left out.
struct LineWriter {
    colors: bool,
    /// The logical line being built.
    line: String,
    /// Length of `line` up to its last cell that shows, and whether a style is in force there.
    keep: usize,
    keep_styled: bool,
    /// The SGR style in force in `line`, and the cell style it was made for.
    style: Option<String>,
    key: Option<StyleKey>,
    /// A row has been added to `line` since it was last ended.
    open: bool,
    /// Empty lines held back until something follows them.
    empty: usize,
}

impl LineWriter {
    fn new(colors: bool) -> Self {
        Self {
            colors,
            line: String::new(),
            keep: 0,
            keep_styled: false,
            style: None,
            key: None,
            open: false,
            empty: 0,
        }
    }

    /// Add a row; `wraps` says it continues on the next row (a soft wrap).
    fn row(&mut self, row: &Row<Cell>, wraps: bool, cols: usize, out: &mut String) {
        for c in 0..cols {
            self.cell(&row[Column(c)]);
        }
        self.open = true;
        if !wraps {
            self.end_line(out);
        }
    }

    fn cell(&mut self, cell: &Cell) {
        if spacer(cell) {
            return;
        }
        if self.colors {
            // Formatting a style costs an allocation; most cells share their neighbour's.
            let key = style_key(cell);
            if self.key != Some(key) {
                self.key = Some(key);
                let next = sgr(cell);
                if next != self.style {
                    self.line.push_str(next.as_deref().unwrap_or("\x1b[0m"));
                    self.style = next;
                }
            }
        }
        push_cell(&mut self.line, cell);
        let shows = !blank(cell) || (self.colors && !plain_style(cell));
        if shows {
            self.keep = self.line.len();
            self.keep_styled = self.style.is_some();
        }
    }

    fn end_line(&mut self, out: &mut String) {
        self.line.truncate(self.keep);
        if self.keep_styled {
            self.line.push_str("\x1b[0m");
        }
        if self.line.is_empty() {
            self.empty += 1;
        } else {
            for _ in 0..self.empty {
                out.push('\n');
            }
            self.empty = 0;
            out.push_str(&self.line);
            out.push('\n');
        }
        self.line.clear();
        self.keep = 0;
        self.keep_styled = false;
        self.style = None;
        self.key = None;
        self.open = false;
    }

    /// End the last line if a wrapped row left it open; drop the empty lines held back.
    fn finish(&mut self, out: &mut String) {
        if self.open {
            self.end_line(out);
        }
    }
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

/// What [`sgr`] reads from a cell.
type StyleKey = (Color, Color, Flags);

fn style_key(cell: &Cell) -> StyleKey {
    let flags = cell.flags
        & (Flags::BOLD
            | Flags::DIM
            | Flags::ITALIC
            | Flags::INVERSE
            | Flags::STRIKEOUT
            | Flags::ALL_UNDERLINES);
    (cell.fg, cell.bg, flags)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GridSize;

    fn term(cols: u16, rows: u16, bytes: &[u8]) -> SharedTerminal {
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
        std::sync::Arc::new(parking_lot::Mutex::new(t))
    }

    fn gunzip(bytes: &[u8]) -> io::Result<String> {
        use std::io::Read;
        let mut s = String::new();
        flate2::read::GzDecoder::new(bytes).read_to_string(&mut s)?;
        Ok(s)
    }

    fn text(t: &SharedTerminal, format: ExportFormat) -> String {
        let mut out = Vec::new();
        export(t, format, &mut out).unwrap();
        String::from_utf8(out).unwrap()
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
        let mut out = Vec::new();
        assert!(
            export(&t, ExportFormat::Plain, &mut out)
                .unwrap()
                .alt_screen
        );
        assert_eq!(out, b"normal 1\nnormal 2\n");
        let t = term(20, 4, b"normal 1\r\n\x1b[?1049hAPP\x1b[?1049l");
        let mut out = Vec::new();
        assert!(
            !export(&t, ExportFormat::Plain, &mut out)
                .unwrap()
                .alt_screen
        );
        assert_eq!(out, b"normal 1\n");
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
            write_file(&path, f, &t).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let back = if f.gzip() {
                assert_eq!(&bytes[..2], [0x1f, 0x8b], "gzip magic");
                gunzip(&bytes).unwrap()
            } else {
                String::from_utf8(bytes).unwrap()
            };
            assert_eq!(back, text);
        }
        // Nothing but the four files: no temporary file is left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_export_leaves_an_existing_file_and_no_temporary_file() {
        let dir = std::env::temp_dir().join(format!("fbterm-export-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keep.txt");
        std::fs::write(&path, "old").unwrap();
        let t = term(20, 3, b"new");
        // Resized between chunks: the export fails and the old file stays as it was.
        let mut grow = || {
            t.lock().resize(GridSize {
                cols: 30,
                rows: 3,
                cell_width: 8.0,
                cell_height: 16.0,
            })
        };
        let mut sink = Vec::new();
        let err = export_chunked(&t, ExportFormat::Plain, &mut sink, 1, &mut grow).unwrap_err();
        assert!(matches!(err, ExportError::Resized));
        // An unwritable target: nothing written, the temporary file removed.
        let missing = dir.join("no such dir").join("x.txt");
        assert!(matches!(
            write_file(&missing, ExportFormat::Plain, &t),
            Err(ExportError::Io(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_between_chunks_neither_repeats_nor_skips_lines() {
        let mut bytes = Vec::new();
        for i in 0..40 {
            bytes.extend(format!("old {i}\r\n").bytes());
        }
        let t = term(20, 5, &bytes);
        // Every chunk boundary, new output scrolls the screen by three lines.
        let mut n = 0;
        let mut more = || {
            let mut b = Vec::new();
            for _ in 0..3 {
                b.extend(format!("new {n}\r\n").bytes());
                n += 1;
            }
            t.lock().feed(&b);
        };
        let mut out = Vec::new();
        export_chunked(&t, ExportFormat::Plain, &mut out, 7, &mut more).unwrap();
        let out = String::from_utf8(out).unwrap();
        let old: Vec<&str> = out.lines().filter(|l| l.starts_with("old")).collect();
        let want: Vec<String> = (0..40).map(|i| format!("old {i}")).collect();
        assert_eq!(old, want);
    }

    #[test]
    fn export_survives_history_clear_midway() {
        let mut t = Terminal::new(
            GridSize {
                cols: 20,
                rows: 5,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            10_000,
        );
        let mut bytes = Vec::new();
        for i in 0..5000 {
            bytes.extend(format!("line {i}\r\n").bytes());
        }
        t.feed(&bytes);
        let t = std::sync::Arc::new(parking_lot::Mutex::new(t));
        // After the first 1000-row chunk, the history is erased (CSI 3 J).
        let mut calls = 0;
        let mut clear = || {
            calls += 1;
            if calls == 1 {
                t.lock().feed(b"\x1b[3J");
            }
        };
        let mut out = Vec::new();
        export_chunked(&t, ExportFormat::Plain, &mut out, 1000, &mut clear).unwrap();
        assert!(calls >= 1, "the export ran in more than one chunk");
        let out = String::from_utf8(out).unwrap();
        let numbers: Vec<usize> = out
            .lines()
            .map(|l| l.strip_prefix("line ").unwrap().parse().unwrap())
            .collect();
        // The rows read before the clear are all there, then only what survived it (the
        // screen), never a row twice or out of order.
        assert_eq!(numbers[..1000], (0..1000).collect::<Vec<_>>()[..]);
        assert!(numbers.windows(2).all(|w| w[0] < w[1]), "{numbers:?}");
        assert_eq!(numbers.last(), Some(&4999));
    }

    #[test]
    fn chunks_join_wrapped_lines_and_keep_styles_across_their_boundaries() {
        // 5 columns, 1-row chunks: every row is its own chunk.
        let t = term(5, 6, b"\x1b[31mabcdefghij\x1b[0mxy   \r\nz");
        let whole = text(&t, ExportFormat::Ansi);
        let mut out = Vec::new();
        export_chunked(&t, ExportFormat::Ansi, &mut out, 1, &mut || {}).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), whole);
        assert_eq!(whole, "\x1b[0;31mabcdefghij\x1b[0mxy\nz\n");
    }

    #[test]
    fn an_empty_terminal_exports_nothing() {
        let t = term(20, 3, b"");
        assert_eq!(text(&t, ExportFormat::Plain), "");
        assert_eq!(text(&t, ExportFormat::Ansi), "");
    }
}
