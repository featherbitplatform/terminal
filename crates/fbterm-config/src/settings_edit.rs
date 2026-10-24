//! Changing one setting in `settings.toml` in place: only that key's value changes; comments,
//! blank lines, the order of keys and every other key stay as they are.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Value};

use crate::settings::{DEFAULT_MAX_FPS, MaxFps, Settings, ThemeMode};
use crate::store::{ConfigError, atomic_write, line_of};

/// One setting with its new value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingChange {
    FontFamily(String),
    FontSize(f32),
    ScrollbackLines(usize),
    Theme(ThemeMode),
    KittyKeyboard(bool),
    BackgroundOpacity(f32),
    BackgroundBlur(bool),
    MaxFps(MaxFps),
}

impl SettingChange {
    /// The key in `settings.toml`.
    pub fn key(&self) -> &'static str {
        match self {
            Self::FontFamily(_) => "font_family",
            Self::FontSize(_) => "font_size",
            Self::ScrollbackLines(_) => "scrollback_lines",
            Self::Theme(_) => "theme",
            Self::KittyKeyboard(_) => "kitty_keyboard",
            Self::BackgroundOpacity(_) => "background_opacity",
            Self::BackgroundBlur(_) => "background_blur",
            Self::MaxFps(_) => "max_fps",
        }
    }

    /// Set this value in `settings`.
    pub fn apply(&self, settings: &mut Settings) {
        match self {
            Self::FontFamily(v) => settings.font_family.clone_from(v),
            Self::FontSize(v) => settings.font_size = *v,
            Self::ScrollbackLines(v) => settings.scrollback_lines = *v,
            Self::Theme(v) => settings.theme = *v,
            Self::KittyKeyboard(v) => settings.kitty_keyboard = *v,
            Self::BackgroundOpacity(v) => settings.background_opacity = *v,
            Self::BackgroundBlur(v) => settings.background_blur = *v,
            Self::MaxFps(v) => settings.max_fps.clone_from(v),
        }
    }

    fn value(&self) -> Value {
        match self {
            Self::FontFamily(v) => Value::from(v.as_str()),
            Self::FontSize(v) | Self::BackgroundOpacity(v) => Value::from(short_f64(*v)),
            Self::ScrollbackLines(v) => Value::from(i64::try_from(*v).unwrap_or(i64::MAX)),
            Self::Theme(ThemeMode::Dark) => Value::from("dark"),
            Self::Theme(ThemeMode::Light) => Value::from("light"),
            Self::KittyKeyboard(v) | Self::BackgroundBlur(v) => Value::from(*v),
            // A number for a fixed rate, the string for the display's.
            Self::MaxFps(MaxFps::Fixed(n)) => Value::from(i64::from(*n)),
            Self::MaxFps(MaxFps::Display) => Value::from("display"),
            Self::MaxFps(MaxFps::Invalid(_)) => Value::from(i64::from(DEFAULT_MAX_FPS)),
        }
    }
}

/// `v` as the shortest decimal that reads back as it (0.85, not 0.8500000238418579).
fn short_f64(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(f64::from(v))
}

impl Settings {
    /// Set one setting in the file at `path`, leaving everything else in it as it is. A missing
    /// file is first created from the commented template. A file that does not load (bad
    /// syntax, or a value of the wrong type) is never written: the error is returned instead.
    /// The write is atomic and keeps the file's permissions. A symlinked `settings.toml` (for
    /// example into a dotfiles folder) stays a symlink: the file it points to is written.
    pub fn write_field(path: &Path, change: &SettingChange) -> Result<(), ConfigError> {
        Self::write_template_if_missing(path)?;
        let resolved = std::fs::canonicalize(path);
        let target = resolved.as_deref().unwrap_or(path);
        let text = std::fs::read_to_string(target)
            .map_err(|e| ConfigError::new(path, None, e.to_string()))?;
        let edited =
            edit_text(&text, change).map_err(|(line, msg)| ConfigError::new(path, line, msg))?;
        if edited == text {
            return Ok(());
        }
        // Errors name the file as the user knows it, not the resolved target.
        atomic_write(target, edited.as_bytes()).map_err(|e| ConfigError::new(path, None, e.message))
    }
}

/// `text` with `change` applied, or the line and message of why `text` does not load.
///
/// An existing `key = value` keeps its place and any trailing comment. A key that is only
/// shown commented out (`#key = ...`, as in the template) is set on the line right after that
/// comment. Otherwise the key is added with the other top-level keys.
pub fn edit_text(text: &str, change: &SettingChange) -> Result<String, (Option<usize>, String)> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        let line = e.span().map(|s| line_of(text, s.start));
        (line, e.message().to_string())
    })?;
    // The file must load as settings too: a value of the wrong type is as broken as bad syntax.
    toml::from_str::<Settings>(text).map_err(|e| {
        let line = e.span().map(|s| line_of(text, s.start));
        (line, e.message().to_string())
    })?;
    let key = change.key();
    let mut value = change.value();
    if let Some(Item::Value(old)) = doc.get_mut(key) {
        *value.decor_mut() = old.decor().clone();
        *old = value;
        return Ok(doc.to_string());
    }
    if let Some(after) = commented_line(text, key) {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let line = format!("{key} = {}", value.to_string().trim());
        let (head, tail) = text.split_at(after);
        let edited = if head.ends_with('\n') {
            format!("{head}{line}{newline}{tail}")
        } else {
            // The comment is the last line and has no line break.
            format!("{head}{newline}{line}{newline}{tail}")
        };
        // The "comment" may really be inside a multi-line string or array: use the line only
        // if the result sets the key at the top level.
        let set_at_top = edited
            .parse::<DocumentMut>()
            .ok()
            .and_then(|d| d.get(key).and_then(Item::as_value).map(ToString::to_string))
            .is_some_and(|v| v.trim() == value.to_string().trim());
        if set_at_top {
            return Ok(edited);
        }
    }
    doc.insert(key, Item::Value(value));
    Ok(doc.to_string())
}

/// The byte offset just past the line `#key = ...` (with its line break), if such a comment
/// comes before any table header.
fn commented_line(text: &str, key: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            return None;
        }
        let is_it = trimmed
            .strip_prefix('#')
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix(key))
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_it {
            return Some(offset);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SETTINGS_TEMPLATE;

    fn every_change() -> Vec<SettingChange> {
        vec![
            SettingChange::FontFamily("JetBrains Mono".into()),
            SettingChange::FontSize(15.5),
            SettingChange::ScrollbackLines(25_000),
            SettingChange::Theme(ThemeMode::Light),
            SettingChange::KittyKeyboard(true),
            SettingChange::BackgroundOpacity(0.85),
            SettingChange::BackgroundBlur(true),
            SettingChange::MaxFps(MaxFps::Fixed(120)),
            SettingChange::MaxFps(MaxFps::Display),
        ]
    }

    #[test]
    fn max_fps_is_written_as_a_number_or_as_display() {
        let after = edit_text(
            SETTINGS_TEMPLATE,
            &SettingChange::MaxFps(MaxFps::Fixed(120)),
        )
        .unwrap();
        assert!(after.contains("#max_fps = 60\nmax_fps = 120\n"), "{after}");
        assert_eq!(
            toml::from_str::<Settings>(&after).unwrap().max_fps,
            MaxFps::Fixed(120)
        );
        let after = edit_text(&after, &SettingChange::MaxFps(MaxFps::Display)).unwrap();
        assert!(
            after.contains("#max_fps = 60\nmax_fps = \"display\"\n"),
            "{after}"
        );
        assert_eq!(
            toml::from_str::<Settings>(&after).unwrap().max_fps,
            MaxFps::Display
        );
        // And back to a number: the type of the value changes, its place does not.
        let after = edit_text(&after, &SettingChange::MaxFps(MaxFps::Fixed(30))).unwrap();
        assert!(after.contains("#max_fps = 60\nmax_fps = 30\n"), "{after}");
    }

    #[test]
    fn an_invalid_max_fps_does_not_block_writing_other_keys() {
        // An unknown word is reported and replaced on load, so the file still loads and edits.
        let text = "max_fps = \"fast\"\n";
        let after = edit_text(text, &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(after, "max_fps = \"fast\"\nfont_size = 15.0\n");
        let after = edit_text(text, &SettingChange::MaxFps(MaxFps::Fixed(90))).unwrap();
        assert_eq!(after, "max_fps = 90\n");
    }

    #[test]
    fn an_existing_key_keeps_its_place_and_comments() {
        let before = "# My settings.\n\
                      theme = \"dark\"   # night owl\n\
                      \n\
                      # Bigger text.\n\
                      font_size = 14.0\n\
                      kitty_keyboard = false\n";
        let after = edit_text(before, &SettingChange::FontSize(16.0)).unwrap();
        assert_eq!(
            after,
            "# My settings.\n\
             theme = \"dark\"   # night owl\n\
             \n\
             # Bigger text.\n\
             font_size = 16.0\n\
             kitty_keyboard = false\n"
        );
        let after = edit_text(before, &SettingChange::Theme(ThemeMode::Light)).unwrap();
        assert_eq!(
            after,
            "# My settings.\n\
             theme = \"light\"   # night owl\n\
             \n\
             # Bigger text.\n\
             font_size = 14.0\n\
             kitty_keyboard = false\n"
        );
    }

    #[test]
    fn a_key_only_shown_in_a_comment_goes_right_after_it() {
        let after = edit_text(SETTINGS_TEMPLATE, &SettingChange::BackgroundOpacity(0.85)).unwrap();
        let expected = SETTINGS_TEMPLATE.replace(
            "#background_opacity = 1.0\n",
            "#background_opacity = 1.0\nbackground_opacity = 0.85\n",
        );
        assert_eq!(after, expected);
        // Setting it again changes the real line, not the comment.
        let again = edit_text(&after, &SettingChange::BackgroundOpacity(0.5)).unwrap();
        assert_eq!(
            again,
            SETTINGS_TEMPLATE.replace(
                "#background_opacity = 1.0\n",
                "#background_opacity = 1.0\nbackground_opacity = 0.5\n",
            )
        );
    }

    #[test]
    fn a_missing_key_joins_the_top_level_keys() {
        let after = edit_text("theme = \"light\"\n", &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(after, "theme = \"light\"\nfont_size = 15.0\n");
        // A commented key after a table header is not where a top-level key can go.
        let text = "theme = \"light\"\n\n[extra]\n#font_size = 13.0\n";
        let after = edit_text(text, &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&after).unwrap().font_size,
            15.0,
            "{after}"
        );
        assert!(after.contains("#font_size = 13.0"));
    }

    #[test]
    fn a_commented_key_inside_a_multi_line_string_is_not_used() {
        let text = "font_family = \"\"\"\n#font_size = 13.0\n\"\"\"\n";
        let after = edit_text(text, &SettingChange::FontSize(15.0)).unwrap();
        let s: Settings = toml::from_str(&after).unwrap();
        assert_eq!(s.font_size, 15.0, "{after}");
        assert_eq!(s.font_family, "#font_size = 13.0\n");
        let text = "theme = \"dark\"\nextra = [\n#font_size = 13.0\n1]\n";
        let after = edit_text(text, &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(toml::from_str::<Settings>(&after).unwrap().font_size, 15.0);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_stays_a_symlink_and_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fbterm-link-{}", uuid::Uuid::new_v4()));
        let dotfiles = dir.join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let real = dotfiles.join("settings.toml");
        std::fs::write(&real, "# mine\ntheme = \"dark\"\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("settings.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        Settings::write_field(&link, &SettingChange::Theme(ThemeMode::Light)).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "# mine\ntheme = \"light\"\n"
        );
        let mode = std::fs::metadata(&real).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_endings_and_a_missing_final_newline_are_respected() {
        let text = "# a\r\n#font_size = 13.0\r\ntheme = \"dark\"\r\n";
        let after = edit_text(text, &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(
            after,
            "# a\r\n#font_size = 13.0\r\nfont_size = 15.0\r\ntheme = \"dark\"\r\n"
        );
        let after = edit_text("#font_size = 13.0", &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(after, "#font_size = 13.0\nfont_size = 15.0\n");
    }

    #[test]
    fn a_broken_file_is_refused() {
        let (line, _) = edit_text(
            "theme = \"dark\"\nfont_size = = 3\n",
            &SettingChange::FontSize(15.0),
        )
        .unwrap_err();
        assert_eq!(line, Some(2));
        let (line, _) =
            edit_text("font_size = \"big\"\n", &SettingChange::KittyKeyboard(true)).unwrap_err();
        assert_eq!(line, Some(1));
    }

    #[test]
    fn every_field_round_trips_through_the_template() {
        let mut text = SETTINGS_TEMPLATE.to_owned();
        let mut expected = Settings::default();
        for change in every_change() {
            text = edit_text(&text, &change).unwrap();
            change.apply(&mut expected);
            assert_eq!(
                toml::from_str::<Settings>(&text).unwrap(),
                expected,
                "{text}"
            );
        }
        // Every comment of the template is still there.
        for line in SETTINGS_TEMPLATE.lines() {
            assert!(text.contains(line), "lost {line:?}");
        }
    }

    #[test]
    fn writing_creates_the_file_from_the_template() {
        let dir = std::env::temp_dir().join(format!("fbterm-edit-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.toml");
        Settings::write_field(&path, &SettingChange::KittyKeyboard(true)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            SETTINGS_TEMPLATE.replace(
                "#kitty_keyboard = false\n",
                "#kitty_keyboard = false\nkitty_keyboard = true\n"
            )
        );
        assert!(Settings::load(&path).unwrap().kitty_keyboard);
        // A broken file is left exactly as it is.
        std::fs::write(&path, "theme = [\n").unwrap();
        let err = Settings::write_field(&path, &SettingChange::FontSize(20.0)).unwrap_err();
        assert_eq!(err.path, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theme = [\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
