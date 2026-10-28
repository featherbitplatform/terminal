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

/// A named color scheme, optionally with a font preference. Serializes as a scheme file
/// (the `builtin` flag is never written; a deserialized scheme is never built-in).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(try_from = "SchemeFile", into = "SchemeFile")]
pub struct Scheme {
    /// Stable id: a built-in id or `user-<uuid>`.
    pub id: String,
    pub name: String,
    pub colors: SchemeColors,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    /// Where the colors came from, for a scheme installed from the gallery or imported
    /// with its credit; written as the `[credit]` table.
    pub credit: Option<Credit>,
    /// Gallery bookkeeping for an installed gallery scheme; written as `[gallery]`.
    pub gallery: Option<GalleryInfo>,
    /// True for the schemes shipped in code; never written to a file.
    pub builtin: bool,
}

/// The `[credit]` table: who made a scheme and under which license. The keys match the
/// published theme catalog.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Credit {
    pub project: String,
    pub author: String,
    pub url: String,
    pub license: String,
    pub copyright: String,
    pub license_file: String,
    pub description: String,
    pub derived: bool,
}

/// The `[gallery]` table: which catalog entry a scheme was installed from.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GalleryInfo {
    /// The catalog id (the scheme id without [`GALLERY_PREFIX`]).
    pub id: String,
    /// The catalog version installed.
    pub version: u32,
    /// [`colors_hash`] of the colors as installed, to tell later edits.
    pub colors_hash: String,
}

/// Scheme ids installed from the gallery start with this.
pub const GALLERY_PREFIX: &str = "gallery-";

/// A stable fingerprint of the 20 colors in file order: FNV-1a 64 over their r, g, b bytes,
/// as 16 lowercase hex digits. Identical across runs and platforms.
pub fn colors_hash(colors: &SchemeColors) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let all = [
        colors.foreground,
        colors.background,
        colors.cursor,
        colors.selection,
    ]
    .into_iter()
    .chain(colors.ansi);
    for c in all {
        for b in [c.r, c.g, c.b] {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// The catalog id of a scheme whose id starts with [`GALLERY_PREFIX`], with or without a
/// `[gallery]` table.
pub fn gallery_id(scheme: &Scheme) -> Option<&str> {
    scheme
        .id
        .strip_prefix(GALLERY_PREFIX)
        .filter(|id| !id.is_empty())
}

/// Whether the colors differ from what the gallery installed: no `[gallery]` table, or a
/// `colors_hash` that no longer matches.
pub fn is_edited(scheme: &Scheme) -> bool {
    scheme
        .gallery
        .as_ref()
        .is_none_or(|g| g.colors_hash != colors_hash(&scheme.colors))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credit: Option<Credit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gallery: Option<GalleryInfo>,
}

impl From<Scheme> for SchemeFile {
    fn from(scheme: Scheme) -> Self {
        Self {
            colors: ColorsFile::from(&scheme.colors),
            id: scheme.id,
            name: scheme.name,
            font_family: scheme.font_family,
            font_size: scheme.font_size,
            credit: scheme.credit,
            gallery: scheme.gallery,
        }
    }
}

impl TryFrom<SchemeFile> for Scheme {
    type Error = String;

    /// Checks what the file format cannot: a non-empty id and name, a positive font size.
    fn try_from(file: SchemeFile) -> Result<Self, String> {
        if file.id.is_empty() || file.name.trim().is_empty() {
            return Err("a scheme needs a non-empty id and name".into());
        }
        if file.font_size.is_some_and(|f| !f.is_finite() || f <= 0.0) {
            return Err("font_size must be a positive number".into());
        }
        Ok(Scheme {
            id: file.id,
            name: file.name,
            colors: file.colors.into(),
            font_family: file.font_family,
            font_size: file.font_size,
            credit: file.credit,
            gallery: file.gallery,
            builtin: false,
        })
    }
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
        credit: None,
        gallery: None,
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
        // Bright black is darker than black and bright white is the ink color, as is
        // usual on a light ground. The design system's light tokens: app canvas, ink text, violet-600 cursor;
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

/// The largest scheme file read: a real one is well under 2 KiB.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Read at most `MAX_FILE_BYTES` from `reader`; `None` when there is more, however the
/// size was reported (a file can grow after its length was checked).
fn read_capped(reader: impl std::io::Read) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut buf = Vec::new();
    reader.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf)?;
    Ok((buf.len() as u64 <= MAX_FILE_BYTES).then_some(buf))
}

/// Read one scheme file. The result is never marked built-in.
pub fn read_file(path: &Path) -> Result<Scheme, ConfigError> {
    let err = |m: String| ConfigError::new(path, None, m);
    let meta = std::fs::metadata(path).map_err(|e| err(e.to_string()))?;
    // A pipe, device or socket reports no size and can be endless: never read one.
    if !meta.is_file() {
        return Err(err("Not a color scheme file.".into()));
    }
    let file = std::fs::File::open(path).map_err(|e| err(e.to_string()))?;
    let bytes = read_capped(file).map_err(|e| err(e.to_string()))?;
    let Some(bytes) = bytes else {
        return Err(err("Not a color scheme file (too large).".into()));
    };
    let text =
        String::from_utf8(bytes).map_err(|_| err("Not a color scheme file (not text).".into()))?;
    let file: SchemeFile = toml::from_str(&text).map_err(|e| {
        let line = e.span().map(|s| line_of(&text, s.start));
        ConfigError::new(path, line, e.message().to_string())
    })?;
    Scheme::try_from(file).map_err(|m| ConfigError::new(path, None, m))
}

/// Write a scheme file atomically. The `builtin` flag is never written.
pub fn write_file(path: &Path, scheme: &Scheme) -> Result<(), ConfigError> {
    let text =
        toml::to_string_pretty(scheme).map_err(|e| ConfigError::new(path, None, e.to_string()))?;
    atomic_write(path, text.as_bytes())
}

/// Whether `id` can name a scheme file: lowercase letters, digits, `-` and `_`, at most 100
/// characters. Lowercase only and length-capped, so ids name the same file on case-insensitive
/// file systems and stay short.
pub fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
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
    let plain = is_plain_id(id);
    if !plain {
        return Err(ConfigError::new(
            &path,
            None,
            format!("invalid scheme id {id:?}"),
        ));
    }
    Ok(path)
}

/// Whether a user scheme from elsewhere (an imported archive) could be a scheme file: a plain,
/// non-built-in id, at most [`MAX_FILE_BYTES`] once written, and loading back unchanged
/// through the scheme file checks.
pub fn is_valid_user(scheme: &Scheme) -> bool {
    if scheme.builtin || check_user_id(Path::new(""), &scheme.id).is_err() {
        return false;
    }
    let Ok(text) = toml::to_string_pretty(scheme) else {
        return false;
    };
    if text.len() as u64 > MAX_FILE_BYTES {
        return false;
    }
    toml::from_str::<Scheme>(&text).is_ok_and(|back| back == *scheme)
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
                Ok(s) => match check_user_id(dir, &s.id) {
                    Err(e) => errors.push(ConfigError::new(&path, None, e.message)),
                    Ok(expected) if expected.file_name() != path.file_name() => {
                        errors.push(ConfigError::new(
                            &path,
                            None,
                            format!("file name must be {}.toml to match id {:?}", s.id, s.id),
                        ));
                    }
                    Ok(_) => schemes.push(s),
                },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_serde_round_trips_and_validates() {
        let mut scheme = builtins().remove(2);
        scheme.font_family = Some("Geist Mono".into());
        scheme.font_size = Some(13.5);
        let text = toml::to_string_pretty(&scheme).unwrap();
        assert!(!text.contains("builtin"));
        let back: Scheme = toml::from_str(&text).unwrap();
        assert_eq!(
            back,
            Scheme {
                builtin: false,
                ..scheme
            }
        );

        let bad = text.replace("font_size = 13.5", "font_size = -1.0");
        assert!(toml::from_str::<Scheme>(&bad).is_err());
    }

    #[test]
    fn valid_user_schemes_follow_the_file_rules() {
        let mut ok = builtins().remove(1);
        ok.builtin = false;
        ok.id = "user-1".into();
        assert!(is_valid_user(&ok));

        let builtin = builtins().remove(0);
        assert!(!is_valid_user(&builtin));
        let builtin_id = Scheme {
            id: "graphite".into(),
            ..ok.clone()
        };
        assert!(!is_valid_user(&builtin_id));
        for id in ["", "../x", "User-1", "a/b", &"x".repeat(101)] {
            let bad = Scheme {
                id: id.into(),
                ..ok.clone()
            };
            assert!(!is_valid_user(&bad), "{id:?}");
        }
        let nameless = Scheme {
            name: " ".into(),
            ..ok.clone()
        };
        assert!(!is_valid_user(&nameless));
        let bad_size = Scheme {
            font_size: Some(f32::NAN),
            ..ok.clone()
        };
        assert!(!is_valid_user(&bad_size));
        let huge = Scheme {
            font_family: Some("f".repeat(MAX_FILE_BYTES as usize)),
            ..ok
        };
        assert!(!is_valid_user(&huge));
    }

    #[test]
    fn growing_read_is_bounded() {
        // An endless reader stops one byte past the cap.
        assert!(read_capped(std::io::repeat(b' ')).unwrap().is_none());
        let at_cap = vec![b'a'; MAX_FILE_BYTES as usize];
        assert_eq!(
            read_capped(&at_cap[..]).unwrap().unwrap().len(),
            at_cap.len()
        );
        let over = vec![b'a'; MAX_FILE_BYTES as usize + 1];
        assert!(read_capped(&over[..]).unwrap().is_none());
    }

    fn sample_credit() -> Credit {
        Credit {
            project: "Nord".into(),
            author: "Arctic Ice Studio".into(),
            url: "https://example.org/nord".into(),
            license: "MIT".into(),
            copyright: "Copyright (c) 2016".into(),
            license_file: "LICENSE".into(),
            description: "An arctic palette.".into(),
            derived: true,
        }
    }

    fn gallery_scheme() -> Scheme {
        let mut s = builtins().remove(0);
        s.builtin = false;
        s.id = "gallery-nord".into();
        s.credit = Some(sample_credit());
        s.gallery = Some(GalleryInfo {
            id: "nord".into(),
            version: 2,
            colors_hash: colors_hash(&s.colors),
        });
        s
    }

    #[test]
    fn credit_and_gallery_round_trip() {
        let scheme = gallery_scheme();
        let text = toml::to_string_pretty(&scheme).unwrap();
        assert!(text.contains("[credit]") && text.contains("[gallery]"));
        assert!(text.contains("license_file = \"LICENSE\"") && text.contains("derived = true"));
        let back: Scheme = toml::from_str(&text).unwrap();
        assert_eq!(back, scheme);
    }

    #[test]
    fn old_files_without_tables_load() {
        let text = toml::to_string_pretty(&builtins().remove(0)).unwrap();
        assert!(!text.contains("[credit]") && !text.contains("[gallery]"));
        let back: Scheme = toml::from_str(&text).unwrap();
        assert!(back.credit.is_none() && back.gallery.is_none());
    }

    #[test]
    fn file_with_tables_loads_in_old_shape() {
        /// A scheme file as read before the tables existed (no `deny_unknown_fields`).
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldSchemeFile {
            id: String,
            name: String,
            font_family: Option<String>,
            font_size: Option<f32>,
            colors: ColorsFile,
        }
        let text = toml::to_string_pretty(&gallery_scheme()).unwrap();
        let old: OldSchemeFile = toml::from_str(&text).unwrap();
        assert_eq!(old.id, "gallery-nord");
    }

    #[test]
    fn colors_hash_is_stable() {
        assert_eq!(
            colors_hash(&builtins().remove(0).colors),
            "210775489270ef4a"
        );
    }

    #[test]
    fn gallery_id_without_block_counts_as_edited() {
        let mut s = gallery_scheme();
        s.gallery = None;
        assert_eq!(gallery_id(&s), Some("nord"));
        assert!(is_edited(&s));
        s.id = "user-1".into();
        assert_eq!(gallery_id(&s), None);
    }

    #[test]
    fn edited_colors_count_as_edited() {
        let mut s = gallery_scheme();
        s.colors.cursor = hex(0x123456);
        assert!(is_edited(&s));
    }

    #[test]
    fn untouched_gallery_scheme_is_not_edited() {
        assert!(!is_edited(&gallery_scheme()));
    }

    #[test]
    fn is_valid_user_accepts_gallery_ids() {
        assert!(is_valid_user(&gallery_scheme()));
    }
}
