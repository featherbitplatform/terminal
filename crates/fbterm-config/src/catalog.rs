//! The theme gallery's catalog: parsing the downloaded `catalog.toml` and telling how each
//! theme relates to the installed schemes.
//!
//! The catalog comes from the network, so it is untrusted input. It is parsed strictly and
//! each theme is checked like a scheme file plus the credit and id rules of the
//! `terminal-themes` repo; a bad theme is skipped with a reason, never fatal. Unknown keys
//! are ignored so the catalog can grow without breaking older versions.

use serde::Deserialize;

use crate::schemes::{
    ColorsFile, Credit, GALLERY_PREFIX, GalleryInfo, Scheme, install_hash, is_edited,
    is_unsafe_char, is_valid_user, unsafe_credit_field,
};

/// The one place the catalog is fetched from.
pub const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/featherbitplatform/terminal-themes/main/catalog.toml";

/// The catalog format this version understands; a higher one is [`CatalogError::NewerFormat`].
pub const CATALOG_FORMAT: u32 = 1;

/// The largest catalog read or parsed.
pub const MAX_CATALOG_BYTES: usize = 512 * 1024;

/// SPDX ids of the permissive licenses a gallery theme may carry.
pub const LICENSES: [&str; 8] = [
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unlicense",
    "CC0-1.0",
    "Zlib",
];

/// The longest string field (name, font family, every credit field) a theme may have.
const MAX_FIELD_CHARS: usize = 2_000;

/// The longest catalog id.
const MAX_ID_CHARS: usize = 64;

/// The longest skip reason kept in [`Catalog::skipped`], in characters.
const MAX_REASON_CHARS: usize = 200;

/// One valid theme of the catalog.
#[derive(Clone, PartialEq, Debug)]
pub struct CatalogTheme {
    /// The catalog id (a slug, without [`GALLERY_PREFIX`]).
    pub id: String,
    pub version: u32,
    /// `variant = "dark"`.
    pub dark: bool,
    /// No longer offered; installed copies stay.
    pub withdrawn: bool,
    /// The scheme as installed: id `gallery-<id>`, with `credit` and `gallery` set.
    pub scheme: Scheme,
}

/// A parsed catalog: the valid themes in catalog order, and why the others were skipped.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Catalog {
    pub themes: Vec<CatalogTheme>,
    /// One line per skipped theme: `"<id or index>: reason"`.
    pub skipped: Vec<String>,
}

/// Why a whole catalog could not be used.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CatalogError {
    /// Not a TOML catalog at all (an HTML error page, binary data, too large, no format).
    NotToml(String),
    /// A catalog format newer than [`CATALOG_FORMAT`].
    NewerFormat(u32),
    /// A catalog with no theme entries.
    Empty,
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotToml(why) => write!(f, "not a theme catalog ({why})"),
            Self::NewerFormat(_) => f.write_str("This catalog needs a newer fbterm."),
            Self::Empty => f.write_str("the catalog has no themes"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// How a catalog theme relates to what is installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Not installed.
    Available,
    /// Installed at the catalog's version.
    Installed,
    /// Installed at an older version, or without a `[gallery]` block; `edited` when the
    /// installed colors no longer match what the gallery installed.
    Update { edited: bool },
    /// No longer offered by the catalog.
    Withdrawn { installed: bool },
}

/// One `[[themes]]` entry as published.
#[derive(Deserialize)]
struct ThemeEntry {
    id: String,
    name: String,
    version: u32,
    variant: String,
    #[serde(default)]
    withdrawn: bool,
    #[serde(default)]
    font_family: Option<String>,
    #[serde(default)]
    font_size: Option<f32>,
    colors: ColorsFile,
    #[serde(default)]
    credit: Option<Credit>,
}

/// Parse a downloaded catalog. Whole-catalog problems are errors; a bad theme is only
/// skipped, with its reason in [`Catalog::skipped`]. Among entries sharing an id, the first
/// valid entry with that id wins; later ones are skipped as duplicates.
pub fn parse_catalog(bytes: &[u8]) -> Result<Catalog, CatalogError> {
    let not_toml = |why: &str| CatalogError::NotToml(why.to_string());
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(not_toml("larger than 512 KiB"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| not_toml("not UTF-8 text"))?;
    if text.trim().is_empty() {
        return Err(CatalogError::Empty);
    }
    let doc: toml::Table = toml::from_str(text).map_err(|e| not_toml(e.message()))?;
    // The format is read before anything else, so a newer catalog is reported as such
    // however its themes changed shape.
    let format = match doc.get("format") {
        Some(toml::Value::Integer(n)) => *n,
        _ => return Err(not_toml("no catalog format")),
    };
    if format > i64::from(CATALOG_FORMAT) {
        return Err(CatalogError::NewerFormat(
            u32::try_from(format).unwrap_or(u32::MAX),
        ));
    }
    if format < 1 {
        return Err(not_toml("unknown catalog format"));
    }
    let entries = match doc.get("themes") {
        None => return Err(CatalogError::Empty),
        Some(toml::Value::Array(entries)) => entries,
        Some(_) => return Err(not_toml("themes is not a list")),
    };
    if entries.is_empty() {
        return Err(CatalogError::Empty);
    }

    let mut catalog = Catalog::default();
    for (index, value) in entries.iter().enumerate() {
        // Name a skipped theme by its id only when that id is a safe slug to print.
        let label = value
            .get("id")
            .and_then(toml::Value::as_str)
            .filter(|id| is_slug(id))
            .map_or_else(|| format!("theme {}", index + 1), str::to_string);
        match theme_from(value) {
            Err(why) => catalog
                .skipped
                .push(format!("{label}: {}", truncate_reason(&why))),
            Ok(theme) if catalog.themes.iter().any(|t| t.id == theme.id) => {
                catalog.skipped.push(format!("{label}: duplicate id"));
            }
            Ok(theme) => catalog.themes.push(theme),
        }
    }
    Ok(catalog)
}

/// A catalog id: lowercase ASCII letters and digits in `-`-separated words, at most
/// [`MAX_ID_CHARS`], and never itself `gallery-` prefixed.
fn is_slug(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_CHARS
        && !id.starts_with(GALLERY_PREFIX)
        && id.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// `why` cut to [`MAX_REASON_CHARS`] characters (on a char boundary) with an ellipsis, so
/// a hostile entry cannot flood the log through an error message that quotes it.
fn truncate_reason(why: &str) -> String {
    match why.char_indices().nth(MAX_REASON_CHARS) {
        None => why.to_string(),
        Some((cut, _)) => format!("{}...", &why[..cut]),
    }
}

/// A `license_file` of the form `LICENSES/<name>`, the name of `[A-Za-z0-9._-]` with no `..`.
fn is_license_file(path: &str) -> bool {
    path.strip_prefix("LICENSES/").is_some_and(|name| {
        !name.is_empty()
            && !name.contains("..")
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    })
}

/// An `https://` URL with a host and no whitespace or control characters.
fn is_https_url(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !rest.chars().any(|c| c.is_whitespace() || c.is_control())
    })
}

/// Check one `[[themes]]` entry and build its scheme.
fn theme_from(value: &toml::Value) -> Result<CatalogTheme, String> {
    let entry: ThemeEntry = value
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_string())?;
    if !is_slug(&entry.id) {
        return Err("invalid id".into());
    }
    if entry.version == 0 {
        return Err("version must be at least 1".into());
    }
    let dark = match entry.variant.as_str() {
        "dark" => true,
        "light" => false,
        _ => return Err("variant must be \"dark\" or \"light\"".into()),
    };
    let Some(credit) = entry.credit else {
        return Err("no credit".into());
    };
    let credit_fields = [
        ("project", &credit.project),
        ("author", &credit.author),
        ("url", &credit.url),
        ("license", &credit.license),
        ("copyright", &credit.copyright),
        ("license_file", &credit.license_file),
        ("description", &credit.description),
    ];
    for (field, text) in credit_fields {
        if text.trim().is_empty() {
            return Err(format!("credit {field} is empty"));
        }
    }
    let long = credit_fields
        .into_iter()
        .chain([("name", &entry.name)])
        .chain(entry.font_family.as_ref().map(|f| ("font_family", f)))
        .find(|(_, text)| text.chars().count() > MAX_FIELD_CHARS);
    if let Some((field, _)) = long {
        return Err(format!(
            "{field} is longer than {MAX_FIELD_CHARS} characters"
        ));
    }
    // Only the description may span lines; nothing may hide or reorder text. The credit
    // rule is the one scheme files and archives get too.
    let unsafe_field = unsafe_credit_field(&credit).or_else(|| {
        [("name", &entry.name)]
            .into_iter()
            .chain(entry.font_family.as_ref().map(|f| ("font_family", f)))
            .find(|(_, text)| text.chars().any(is_unsafe_char))
            .map(|(field, _)| field)
    });
    if let Some(field) = unsafe_field {
        return Err(format!("{field} has control or invisible characters"));
    }
    if !LICENSES.contains(&credit.license.as_str()) {
        return Err(format!(
            "license {:?} is not on the allowlist",
            credit.license
        ));
    }
    if !is_license_file(&credit.license_file) {
        return Err("license_file is not LICENSES/<name>".into());
    }
    if !is_https_url(&credit.url) {
        return Err("credit url is not https://".into());
    }

    let mut scheme = Scheme {
        id: format!("{GALLERY_PREFIX}{}", entry.id),
        name: entry.name,
        colors: entry.colors.into(),
        font_family: entry.font_family,
        font_size: entry.font_size,
        credit: Some(credit),
        gallery: None,
        builtin: false,
    };
    scheme.gallery = Some(GalleryInfo {
        id: entry.id.clone(),
        version: entry.version,
        colors_hash: install_hash(&scheme),
    });
    if !is_valid_user(&scheme) {
        return Err("not a valid color scheme".into());
    }
    Ok(CatalogTheme {
        id: entry.id,
        version: entry.version,
        dark,
        withdrawn: entry.withdrawn,
        scheme,
    })
}

/// The status of `theme`, given the installed scheme with id `gallery-<id>`, if any.
///
/// An installed scheme without a `[gallery]` block for this theme (saved by an older fbterm,
/// or with a block naming another theme) has no known version or colors: it is always an
/// edited update.
pub fn status(theme: &CatalogTheme, installed: Option<&Scheme>) -> Status {
    if theme.withdrawn {
        return Status::Withdrawn {
            installed: installed.is_some(),
        };
    }
    let Some(scheme) = installed else {
        return Status::Available;
    };
    match scheme.gallery.as_ref().filter(|g| g.id == theme.id) {
        None => Status::Update { edited: true },
        Some(g) if g.version >= theme.version => Status::Installed,
        Some(_) => Status::Update {
            edited: is_edited(scheme),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two entries copied from the published catalog (`one-dark` and `tokyo-night-day`), so
    /// this parser stays pinned to the shape `terminal-themes` publishes.
    const PUBLISHED: &str = r##"format = 1

[[themes]]
id = "one-dark"
name = "One Dark"
version = 1
variant = "dark"

[themes.colors]
foreground = "#abb2bf"
background = "#282c34"
cursor = "#528bff"
selection = "#3e4451"
black = "#3e4451"
red = "#e06c75"
green = "#98c379"
yellow = "#e5c07b"
blue = "#61afef"
magenta = "#c678dd"
cyan = "#56b6c2"
white = "#abb2bf"
bright_black = "#5c6370"
bright_red = "#e06c75"
bright_green = "#98c379"
bright_yellow = "#e5c07b"
bright_blue = "#61afef"
bright_magenta = "#c678dd"
bright_cyan = "#56b6c2"
bright_white = "#abb2bf"

[themes.credit]
project = "One Dark and One Light"
author = "GitHub Inc."
url = "https://github.com/atom/atom/tree/master/packages/one-dark-syntax"
license = "MIT"
copyright = "Copyright (c) 2016 GitHub Inc."
license_file = "LICENSES/atom-one-MIT.txt"
description = "Dark syntax theme. The palette has no terminal colors; the 16 are a mapping of it."
derived = true

[[themes]]
id = "tokyo-night-day"
name = "Tokyo Night Day"
version = 1
variant = "light"

[themes.colors]
foreground = "#3760bf"
background = "#e1e2e7"
cursor = "#3760bf"
selection = "#b7c1e3"
black = "#b4b5b9"
red = "#f52a65"
green = "#587539"
yellow = "#8c6c3e"
blue = "#2e7de9"
magenta = "#9854f1"
cyan = "#007197"
white = "#6172b0"
bright_black = "#a1a6c5"
bright_red = "#ff4774"
bright_green = "#5c8524"
bright_yellow = "#a27629"
bright_blue = "#358aff"
bright_magenta = "#a463ff"
bright_cyan = "#007ea8"
bright_white = "#3760bf"

[themes.credit]
project = "Tokyo Night"
author = "Enkia; Folke Lemaitre (tokyonight.nvim)"
url = "https://github.com/folke/tokyonight.nvim"
license = "Apache-2.0"
copyright = "Copyright (c) 2018-present Enkia"
license_file = "LICENSES/tokyonight-nvim-Apache-2.0.txt"
description = "Tokyo Night, Day style."
derived = false
"##;

    /// One theme entry with every field, for building test catalogs.
    fn entry(id: &str) -> String {
        format!(
            r##"
[[themes]]
id = "{id}"
name = "Theme {id}"
version = 3
variant = "dark"
future_key = "ignored"

[themes.colors]
foreground = "#f5f5f7"
background = "#0b0b0e"
cursor = "#a584ff"
selection = "#392f56"
black = "#262629"
red = "#ed4a49"
green = "#2bbb71"
yellow = "#f2a635"
blue = "#70b3f7"
magenta = "#a584ff"
cyan = "#59c5f5"
white = "#d0d0d9"
bright_black = "#919199"
bright_red = "#fd736d"
bright_green = "#52cd86"
bright_yellow = "#fac053"
bright_blue = "#9ccbfa"
bright_magenta = "#bea8ff"
bright_cyan = "#8fd6f7"
bright_white = "#f5f5f7"

[themes.credit]
project = "Project {id}"
author = "Someone"
url = "https://example.org/{id}"
license = "MIT"
copyright = "Copyright (c) 2026 Someone"
license_file = "LICENSES/{id}-MIT.txt"
description = "A theme."
derived = false
"##
        )
    }

    fn catalog_of(entries: &[String]) -> String {
        format!("format = 1\n{}", entries.concat())
    }

    fn parse(text: &str) -> Catalog {
        parse_catalog(text.as_bytes()).unwrap()
    }

    /// A catalog of `entry(id)` with one line replaced: exactly one theme, skipped.
    fn assert_skipped(id: &str, from: &str, to: &str) {
        let text = catalog_of(&[entry(id)]);
        assert!(text.contains(from), "{from:?}");
        let catalog = parse(&text.replacen(from, to, 1));
        assert!(catalog.themes.is_empty(), "{to:?} was accepted");
        assert_eq!(catalog.skipped.len(), 1, "{:?}", catalog.skipped);
    }

    #[test]
    fn parses_a_valid_catalog() {
        let catalog = parse(PUBLISHED);
        assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
        let ids: Vec<&str> = catalog.themes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["one-dark", "tokyo-night-day"]);

        let one = &catalog.themes[0];
        assert_eq!((one.version, one.dark, one.withdrawn), (1, true, false));
        assert_eq!(one.scheme.id, "gallery-one-dark");
        assert_eq!(one.scheme.name, "One Dark");
        assert!(!one.scheme.builtin);
        assert_eq!(one.scheme.colors.background.to_string(), "#282c34");
        assert_eq!(one.scheme.colors.ansi[8].to_string(), "#5c6370");
        let credit = one.scheme.credit.as_ref().unwrap();
        assert_eq!(credit.license, "MIT");
        assert_eq!(credit.license_file, "LICENSES/atom-one-MIT.txt");
        assert!(credit.derived);
        assert_eq!(
            one.scheme.gallery,
            Some(GalleryInfo {
                id: "one-dark".into(),
                version: 1,
                colors_hash: install_hash(&one.scheme),
            })
        );
        assert!(is_valid_user(&one.scheme));
        assert!(!is_edited(&one.scheme));

        let day = &catalog.themes[1];
        assert!(!day.dark);
        assert_eq!(day.scheme.credit.as_ref().unwrap().license, "Apache-2.0");

        // Unknown keys and a withdrawn marker are fine.
        let withdrawn = entry("old").replace("version = 3", "version = 3\nwithdrawn = true");
        let catalog = parse(&catalog_of(&[entry("a"), withdrawn]));
        assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
        assert!(!catalog.themes[0].withdrawn);
        assert!(catalog.themes[1].withdrawn);
        assert_eq!(catalog.themes[1].version, 3);
    }

    #[test]
    fn html_body_is_an_error() {
        let html =
            b"<!DOCTYPE html>\n<html><head><title>404</title></head><body>Not Found</body></html>";
        assert!(matches!(parse_catalog(html), Err(CatalogError::NotToml(_))));
        assert!(matches!(
            parse_catalog(&[0xff, 0xfe, 0x00]),
            Err(CatalogError::NotToml(_))
        ));
        // A TOML document without a format is not a catalog either.
        assert!(matches!(
            parse_catalog(entry("a").as_bytes()),
            Err(CatalogError::NotToml(_))
        ));
        let too_big = format!(
            "{}# {}\n",
            catalog_of(&[entry("a")]),
            "x".repeat(MAX_CATALOG_BYTES)
        );
        assert!(matches!(
            parse_catalog(too_big.as_bytes()),
            Err(CatalogError::NotToml(_))
        ));
        assert_eq!(parse_catalog(b""), Err(CatalogError::Empty));
        assert_eq!(parse_catalog(b"format = 1\n"), Err(CatalogError::Empty));
    }

    #[test]
    fn newer_format_is_reported() {
        let text = catalog_of(&[entry("a")]).replace("format = 1", "format = 2");
        assert_eq!(
            parse_catalog(text.as_bytes()),
            Err(CatalogError::NewerFormat(2))
        );
        // A newer format is reported even when its themes changed shape.
        assert_eq!(
            parse_catalog(b"format = 2\nthemes = \"elsewhere\"\n"),
            Err(CatalogError::NewerFormat(2))
        );
        assert!(matches!(
            parse_catalog(b"format = 0\n"),
            Err(CatalogError::NotToml(_))
        ));
    }

    #[test]
    fn invalid_theme_is_skipped_not_fatal() {
        let bad = entry("bad").replace("red = \"#ed4a49\"", "red = \"#12345\"");
        let catalog = parse(&catalog_of(&[bad, entry("good")]));
        assert_eq!(catalog.themes.len(), 1);
        assert_eq!(catalog.themes[0].id, "good");
        assert_eq!(catalog.skipped.len(), 1);
        assert!(
            catalog.skipped[0].starts_with("bad: "),
            "{:?}",
            catalog.skipped
        );

        // A theme that is not even a table is named by its index.
        let catalog = parse("format = 1\nthemes = [1, { id = 2 }]\n");
        assert!(catalog.themes.is_empty());
        assert_eq!(catalog.skipped.len(), 2);
        assert!(
            catalog.skipped[0].starts_with("theme 1: "),
            "{:?}",
            catalog.skipped
        );
        assert!(
            catalog.skipped[1].starts_with("theme 2: "),
            "{:?}",
            catalog.skipped
        );

        // Other scheme-file rules: a name, a known variant, a version, a sane font size.
        assert_skipped("a", "name = \"Theme a\"", "name = \" \"");
        assert_skipped("a", "variant = \"dark\"", "variant = \"dim\"");
        assert_skipped("a", "version = 3", "version = 0");
        assert_skipped("a", "version = 3", "version = -1");
        assert_skipped("a", "version = 3", "font_size = -2.0\nversion = 3");
    }

    #[test]
    fn license_off_allowlist_is_skipped() {
        for license in ["GPL-3.0-only", "mit", "", "Proprietary"] {
            assert_skipped(
                "a",
                "license = \"MIT\"",
                &format!("license = \"{license}\""),
            );
        }
        for license in LICENSES {
            let text = catalog_of(&[entry("a")])
                .replace("license = \"MIT\"", &format!("license = \"{license}\""));
            assert_eq!(parse(&text).themes.len(), 1, "{license}");
        }
    }

    #[test]
    fn missing_credit_is_skipped() {
        let text = catalog_of(&[entry("a")]);
        let start = text.find("[themes.credit]").unwrap();
        let catalog = parse(&text[..start]);
        assert!(catalog.themes.is_empty());
        assert_eq!(catalog.skipped.len(), 1);

        for field in [
            "project",
            "author",
            "url",
            "copyright",
            "license_file",
            "description",
        ] {
            // Absent, and present but blank.
            let line = text
                .lines()
                .find(|l| l.starts_with(&format!("{field} = ")))
                .unwrap();
            assert_skipped("a", line, "");
            assert_skipped("a", line, &format!("{field} = \"  \""));
        }
        assert_skipped("a", "derived = false", "");
    }

    #[test]
    fn non_https_credit_url_is_skipped() {
        for url in [
            "http://example.org/a",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "HTTPS://example.org",
            "https://",
            "https:// spaced.example",
        ] {
            assert_skipped(
                "a",
                "url = \"https://example.org/a\"",
                &format!("url = \"{url}\""),
            );
        }
    }

    #[test]
    fn bad_id_is_skipped() {
        for id in [
            "../x",
            "Upper",
            "gallery-x",
            "",
            "a_b",
            "-a",
            "a-",
            "a--b",
            "a b",
            "a/b",
            &"x".repeat(MAX_ID_CHARS + 1),
        ] {
            assert_skipped("a", "id = \"a\"", &format!("id = \"{id}\""));
        }
        let max = "x".repeat(MAX_ID_CHARS);
        assert_eq!(parse(&catalog_of(&[entry(&max)])).themes.len(), 1);
        assert_skipped("a", "id = \"a\"", "id = 7");
    }

    #[test]
    fn oversized_fields_are_skipped() {
        let long = "y".repeat(10_000);
        assert_skipped("a", "name = \"Theme a\"", &format!("name = \"{long}\""));
        assert_skipped(
            "a",
            "description = \"A theme.\"",
            &format!("description = \"{long}\""),
        );
        assert_skipped(
            "a",
            "version = 3",
            &format!("version = 3\nfont_family = \"{long}\""),
        );
        let at_limit = "y".repeat(MAX_FIELD_CHARS);
        let text = catalog_of(&[entry("a")]).replace(
            "description = \"A theme.\"",
            &format!("description = \"{at_limit}\""),
        );
        assert_eq!(parse(&text).themes.len(), 1);
    }

    #[test]
    fn hidden_characters_are_skipped() {
        for bad in [
            "a\u{1b}[31mb",
            "a\u{202e}b",
            "a\nb",
            "a\u{200b}b",
            "\u{feff}a",
            "a\u{2066}b",
            "a\u{2060}b",
            "soft\u{00AD}hyphen",
            "a\u{061C}b",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{180E}b",
        ] {
            let toml_str = toml::Value::String(bad.into()).to_string();
            assert_skipped("a", "name = \"Theme a\"", &format!("name = {toml_str}"));
            assert_skipped("a", "author = \"Someone\"", &format!("author = {toml_str}"));
            assert_skipped(
                "a",
                "version = 3",
                &format!("version = 3\nfont_family = {toml_str}"),
            );
        }
        assert_skipped(
            "a",
            "description = \"A theme.\"",
            "description = \"a\\u202eb\"",
        );
        // A newline is fine in the description, and only there.
        let text = catalog_of(&[entry("a")]).replace(
            "description = \"A theme.\"",
            "description = \"Line one.\\nLine two.\"",
        );
        let catalog = parse(&text);
        assert_eq!(catalog.themes.len(), 1, "{:?}", catalog.skipped);
        assert_eq!(
            catalog.themes[0]
                .scheme
                .credit
                .as_ref()
                .unwrap()
                .description,
            "Line one.\nLine two."
        );
    }

    #[test]
    fn license_file_must_be_under_licenses() {
        for path in [
            "LICENSE",
            "LICENSES/",
            "LICENSES/../x",
            "LICENSES/a..b",
            "LICENSES/sub/x.txt",
            "/etc/passwd",
            "licenses/x.txt",
            "LICENSES/x y.txt",
        ] {
            assert_skipped(
                "a",
                "license_file = \"LICENSES/a-MIT.txt\"",
                &format!("license_file = \"{path}\""),
            );
        }
        let text = catalog_of(&[entry("a")])
            .replace("LICENSES/a-MIT.txt", "LICENSES/Some_Project-2.0.MIT.txt");
        assert_eq!(parse(&text).themes.len(), 1);
    }

    #[test]
    fn long_skip_reasons_are_truncated() {
        let huge = "z".repeat(100_000);
        let bad = entry("a").replace("red = \"#ed4a49\"", &format!("red = \"{huge}\""));
        let catalog = parse(&catalog_of(&[bad]));
        assert_eq!(catalog.skipped.len(), 1);
        let line = &catalog.skipped[0];
        assert!(line.starts_with("a: "), "{line}");
        assert!(line.ends_with("..."), "{line}");
        assert!(line.chars().count() <= "a: ".len() + MAX_REASON_CHARS + 3);
        // Cuts on a char boundary, multibyte text included.
        let cut = truncate_reason(&"\u{e9}".repeat(1_000));
        assert_eq!(cut.chars().count(), MAX_REASON_CHARS + 3);
        assert_eq!(truncate_reason("short"), "short");
    }

    #[test]
    fn duplicate_ids_keep_the_first() {
        let second = entry("a").replace("name = \"Theme a\"", "name = \"Second\"");
        let catalog = parse(&catalog_of(&[entry("a"), second, entry("b")]));
        let names: Vec<&str> = catalog
            .themes
            .iter()
            .map(|t| t.scheme.name.as_str())
            .collect();
        assert_eq!(names, ["Theme a", "Theme b"]);
        assert_eq!(catalog.skipped.len(), 1);
        assert!(
            catalog.skipped[0].starts_with("a: "),
            "{:?}",
            catalog.skipped
        );
    }

    #[test]
    fn status_cases() {
        let catalog = parse(&catalog_of(&[entry("a")]));
        let theme = &catalog.themes[0];
        assert_eq!(theme.version, 3);

        assert_eq!(status(theme, None), Status::Available);

        let same = theme.scheme.clone();
        assert_eq!(status(theme, Some(&same)), Status::Installed);

        let mut older = theme.scheme.clone();
        older.gallery.as_mut().unwrap().version = 2;
        assert_eq!(
            status(theme, Some(&older)),
            Status::Update { edited: false }
        );

        let mut edited = older.clone();
        edited.colors.background = edited.colors.foreground;
        assert_eq!(
            status(theme, Some(&edited)),
            Status::Update { edited: true }
        );

        // An edited theme at the current version is still installed: nothing newer to offer.
        let mut edited_same = same.clone();
        edited_same.colors.background = edited_same.colors.foreground;
        assert_eq!(status(theme, Some(&edited_same)), Status::Installed);

        // A `gallery-` file without its block has no known version: always an edited update,
        // even with the catalog's exact colors.
        let mut blockless = theme.scheme.clone();
        blockless.gallery = None;
        assert_eq!(
            status(theme, Some(&blockless)),
            Status::Update { edited: true }
        );

        let withdrawn = CatalogTheme {
            withdrawn: true,
            ..theme.clone()
        };
        assert_eq!(
            status(&withdrawn, None),
            Status::Withdrawn { installed: false }
        );
        assert_eq!(
            status(&withdrawn, Some(&same)),
            Status::Withdrawn { installed: true }
        );
    }
}
