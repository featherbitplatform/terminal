//! Color schemes: the data model, the built-in schemes and the on-disk store.
//!
//! A scheme is one plain TOML file per scheme in `<config>/schemes/`, named
//! `<id>.toml`, with colors as `"#rrggbb"`. Built-in schemes live in code and are
//! never written to disk. Profiles and settings refer to a scheme by its stable id.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profile::Rgb;
use crate::store::{ConfigError, atomic_write, line_of};

/// The colors of a scheme. `selection` is the selection background, already
/// flattened over `background`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SchemeColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

/// A named color scheme, optionally with a font preference.
#[derive(Clone, PartialEq, Debug)]
pub struct Scheme {
    /// Stable id: a built-in id or `user-<uuid>`.
    pub id: String,
    pub name: String,
    pub colors: SchemeColors,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    /// True for the schemes shipped in code; never written to a file.
    pub builtin: bool,
}

/// Ids of the built-in schemes, fixed forever.
pub const BUILTIN_IDS: [&str; 4] = ["graphite", "graphite-light", "harbor", "ember"];

/// The `[colors]` table of a scheme file.
#[derive(Serialize, Deserialize)]
struct ColorsFile {
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
    selection: Rgb,
    black: Rgb,
    red: Rgb,
    green: Rgb,
    yellow: Rgb,
    blue: Rgb,
    magenta: Rgb,
    cyan: Rgb,
    white: Rgb,
    bright_black: Rgb,
    bright_red: Rgb,
    bright_green: Rgb,
    bright_yellow: Rgb,
    bright_blue: Rgb,
    bright_magenta: Rgb,
    bright_cyan: Rgb,
    bright_white: Rgb,
}

/// The shape of a scheme file.
#[derive(Serialize, Deserialize)]
struct SchemeFile {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size: Option<f32>,
    colors: ColorsFile,
}

impl From<&SchemeColors> for ColorsFile {
    fn from(c: &SchemeColors) -> Self {
        let a = &c.ansi;
        Self {
            foreground: c.foreground,
            background: c.background,
            cursor: c.cursor,
            selection: c.selection,
            black: a[0],
            red: a[1],
            green: a[2],
            yellow: a[3],
            blue: a[4],
            magenta: a[5],
            cyan: a[6],
            white: a[7],
            bright_black: a[8],
            bright_red: a[9],
            bright_green: a[10],
            bright_yellow: a[11],
            bright_blue: a[12],
            bright_magenta: a[13],
            bright_cyan: a[14],
            bright_white: a[15],
        }
    }
}

impl From<ColorsFile> for SchemeColors {
    fn from(c: ColorsFile) -> Self {
        Self {
            foreground: c.foreground,
            background: c.background,
            cursor: c.cursor,
            selection: c.selection,
            ansi: [
                c.black,
                c.red,
                c.green,
                c.yellow,
                c.blue,
                c.magenta,
                c.cyan,
                c.white,
                c.bright_black,
                c.bright_red,
                c.bright_green,
                c.bright_yellow,
                c.bright_blue,
                c.bright_magenta,
                c.bright_cyan,
                c.bright_white,
            ],
        }
    }
}

const fn hex(v: u32) -> Rgb {
    Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    }
}

fn builtin(
    id: &str,
    name: &str,
    foreground: u32,
    background: u32,
    cursor: u32,
    selection: u32,
    ansi: [u32; 16],
) -> Scheme {
    Scheme {
        id: id.to_string(),
        name: name.to_string(),
        colors: SchemeColors {
            foreground: hex(foreground),
            background: hex(background),
            cursor: hex(cursor),
            selection: hex(selection),
            ansi: ansi.map(hex),
        },
        font_family: None,
        font_size: None,
        builtin: true,
    }
}

/// The four built-in schemes, in `BUILTIN_IDS` order.
pub fn builtins() -> Vec<Scheme> {
    vec![
        // The design system's dark Graphite palette; selection is violet-400 at 30%
        // over the background.
        builtin(
            "graphite",
            "Graphite",
            0xf5f5f7,
            0x0b0b0e,
            0xa584ff,
            0x392f56,
            [
                0x262629, 0xed4a49, 0x2bbb71, 0xf2a635, 0x70b3f7, 0xa584ff, 0x59c5f5, 0xd0d0d9,
                0x919199, 0xfd736d, 0x52cd86, 0xfac053, 0x9ccbfa, 0xbea8ff, 0x8fd6f7, 0xf5f5f7,
            ],
        ),
        // The design system's light tokens: app canvas, ink text, violet-600 cursor;
        // status colors darkened until they pass on the light canvas.
        builtin(
            "graphite-light",
            "Graphite Light",
            0x1c1c22,
            0xfafafb,
            0x7c49e3,
            0xe1d7f6,
            [
                0x4f4f56, 0xc52b30, 0x00774a, 0x8f5700, 0x1a64c4, 0x7c49e3, 0x0b6f94, 0x686870,
                0x3a3a42, 0xb01f2a, 0x006b41, 0x7d4c00, 0x1556ad, 0x6738c1, 0x0a6384, 0x1c1c22,
            ],
        ),
        // Cool blue-green: deep teal-black ground, aqua cursor.
        builtin(
            "harbor",
            "Harbor",
            0xe3f2f4,
            0x0a1a20,
            0x4fd1c5,
            0x1d4d4e,
            [
                0x17313a, 0xf0716c, 0x4cc9a0, 0xe5c15d, 0x5aa9e6, 0x9d8cf0, 0x3fd0d8, 0xc3dadd,
                0x5f8089, 0xff918c, 0x73ddb8, 0xf2d683, 0x85c3f2, 0xb9adf7, 0x78e2e8, 0xf4fbfc,
            ],
        ),
        // Warm red-orange on a coal-brown ground; its look makes a production host
        // stand out.
        builtin(
            "ember",
            "Ember",
            0xf6ece4,
            0x1a0f0c,
            0xff8a4c,
            0x5a311e,
            [
                0x3a2520, 0xff5f56, 0x9fcf5a, 0xffb347, 0x6fa8dc, 0xd98cb3, 0x4fc3b0, 0xd9c5b8,
                0x8a6a5e, 0xff857c, 0xb8e07a, 0xffc872, 0x94c0e8, 0xe8a8c8, 0x7ad6c6, 0xfff6ee,
            ],
        ),
    ]
}

fn luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG 2 contrast ratio between two colors (1.0 to 21.0).
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a scheme meets the readability bar the built-ins are held to: foreground
/// against background at least 7.0, ANSI colors 1 to 6 and 9 to 14 against the
/// background at least 4.5, and the selection against the foreground at least 4.5.
pub fn contrast_ok(colors: &SchemeColors) -> bool {
    let bg = colors.background;
    contrast_ratio(colors.foreground, bg) >= 7.0
        && (1..=6)
            .chain(9..=14)
            .all(|i| contrast_ratio(colors.ansi[i], bg) >= 4.5)
        && contrast_ratio(colors.selection, colors.foreground) >= 4.5
}

/// A fresh id for a user scheme: `user-<uuid v4>`.
pub fn new_user_id() -> String {
    format!("user-{}", Uuid::new_v4())
}

/// Read one scheme file. The result is never marked built-in.
pub fn read_file(path: &Path) -> Result<Scheme, ConfigError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| ConfigError::new(path, None, e.to_string()))?;
    let file: SchemeFile = toml::from_str(&text).map_err(|e| {
        let line = e.span().map(|s| line_of(&text, s.start));
        ConfigError::new(path, line, e.message().to_string())
    })?;
    if file.id.is_empty() || file.name.trim().is_empty() {
        return Err(ConfigError::new(
            path,
            None,
            "a scheme needs a non-empty id and name".into(),
        ));
    }
    Ok(Scheme {
        id: file.id,
        name: file.name,
        colors: file.colors.into(),
        font_family: file.font_family,
        font_size: file.font_size,
        builtin: false,
    })
}

/// Write a scheme file atomically. The `builtin` flag is never written.
pub fn write_file(path: &Path, scheme: &Scheme) -> Result<(), ConfigError> {
    let file = SchemeFile {
        id: scheme.id.clone(),
        name: scheme.name.clone(),
        font_family: scheme.font_family.clone(),
        font_size: scheme.font_size,
        colors: ColorsFile::from(&scheme.colors),
    };
    let text =
        toml::to_string_pretty(&file).map_err(|e| ConfigError::new(path, None, e.to_string()))?;
    atomic_write(path, text.as_bytes())
}

/// Ids become file names, so only plain characters are allowed.
fn check_user_id(dir: &Path, id: &str) -> Result<PathBuf, ConfigError> {
    let path = dir.join(format!("{id}.toml"));
    if BUILTIN_IDS.contains(&id) {
        return Err(ConfigError::new(
            &path,
            None,
            format!("{id:?} is a built-in scheme"),
        ));
    }
    let plain = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !plain {
        return Err(ConfigError::new(
            &path,
            None,
            format!("invalid scheme id {id:?}"),
        ));
    }
    Ok(path)
}

/// Save a user scheme as `<dir>/<id>.toml`. Built-ins are refused.
pub fn save_user(dir: &Path, scheme: &Scheme) -> Result<PathBuf, ConfigError> {
    let path = check_user_id(dir, &scheme.id)?;
    if scheme.builtin {
        return Err(ConfigError::new(
            &path,
            None,
            "built-in schemes are not saved".into(),
        ));
    }
    write_file(&path, scheme)?;
    Ok(path)
}

/// Delete a user scheme's file. Built-ins are refused; a missing file is fine.
pub fn delete_user(dir: &Path, id: &str) -> Result<(), ConfigError> {
    let path = check_user_id(dir, id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ConfigError::new(&path, None, e.to_string())),
    }
}

/// Every scheme available: the built-ins, then the user files.
#[derive(Clone, PartialEq, Debug)]
pub struct SchemeSet {
    schemes: Vec<Scheme>,
}

impl SchemeSet {
    /// Load the built-ins and every `*.toml` file in `dir`, sorted by file name. A
    /// missing directory means no user schemes. A file that does not parse, uses a
    /// built-in id or repeats an earlier id is skipped with an error.
    pub fn load(dir: &Path) -> (Self, Vec<ConfigError>) {
        let mut schemes = builtins();
        let mut errors = Vec::new();
        let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.is_file())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                errors.push(ConfigError::new(dir, None, e.to_string()));
                Vec::new()
            }
        };
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for path in files {
            match read_file(&path) {
                Err(e) => errors.push(e),
                Ok(s) if BUILTIN_IDS.contains(&s.id.as_str()) => errors.push(ConfigError::new(
                    &path,
                    None,
                    format!("{:?} is a built-in scheme id", s.id),
                )),
                Ok(s) if schemes.iter().any(|o| o.id == s.id) => errors.push(ConfigError::new(
                    &path,
                    None,
                    format!("duplicate scheme id {:?}", s.id),
                )),
                Ok(s) => schemes.push(s),
            }
        }
        (Self { schemes }, errors)
    }

    pub fn get(&self, id: &str) -> Option<&Scheme> {
        self.schemes.iter().find(|s| s.id == id)
    }

    pub fn all(&self) -> &[Scheme] {
        &self.schemes
    }
}
