//! Changing one setting in `settings.toml` in place: only that key's value changes; comments,
//! blank lines, the order of keys and every other key stay as they are.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Value};

use crate::keys::{Action, Overrides};
use crate::settings::{
    DEFAULT_MAX_FPS, MaxFps, SecretStorage, Settings, ThemeMode, WindowDecorations,
};
use crate::store::{ConfigError, atomic_write, line_of};

/// One setting with its new value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingChange {
    FontFamily(String),
    FontSize(f32),
    ScrollbackLines(usize),
    Theme(ThemeMode),
    ColorScheme(String),
    KittyKeyboard(bool),
    BackgroundOpacity(f32),
    BackgroundBlur(bool),
    WindowDecorations(WindowDecorations),
    MaxFps(MaxFps),
    TunnelMaxRetries(u32),
    SecretStorage(SecretStorage),
    /// Every keyboard shortcut override (the whole `[keys]` table as the app sees it). Only
    /// the actions whose chord differs from what the file gives are written, so entries the
    /// app could not take (and other text in the table) stay as they are unless that action
    /// is changed.
    Keys(Overrides),
}

impl SettingChange {
    /// The key in `settings.toml`.
    pub fn key(&self) -> &'static str {
        match self {
            Self::FontFamily(_) => "font_family",
            Self::FontSize(_) => "font_size",
            Self::ScrollbackLines(_) => "scrollback_lines",
            Self::Theme(_) => "theme",
            Self::ColorScheme(_) => "color_scheme",
            Self::KittyKeyboard(_) => "kitty_keyboard",
            Self::BackgroundOpacity(_) => "background_opacity",
            Self::BackgroundBlur(_) => "background_blur",
            Self::WindowDecorations(_) => "window_decorations",
            Self::MaxFps(_) => "max_fps",
            Self::TunnelMaxRetries(_) => "tunnel_max_retries",
            Self::SecretStorage(_) => "secret_storage",
            Self::Keys(_) => "keys",
        }
    }

    /// Set this value in `settings`.
    pub fn apply(&self, settings: &mut Settings) {
        match self {
            Self::FontFamily(v) => settings.font_family.clone_from(v),
            Self::FontSize(v) => settings.font_size = *v,
            Self::ScrollbackLines(v) => settings.scrollback_lines = *v,
            Self::Theme(v) => settings.theme = *v,
            Self::ColorScheme(v) => settings.color_scheme.clone_from(v),
            Self::KittyKeyboard(v) => settings.kitty_keyboard = *v,
            Self::BackgroundOpacity(v) => settings.background_opacity = *v,
            Self::BackgroundBlur(v) => settings.background_blur = *v,
            Self::WindowDecorations(v) => settings.window_decorations = *v,
            Self::MaxFps(v) => settings.max_fps.clone_from(v),
            Self::TunnelMaxRetries(v) => settings.tunnel_max_retries = *v,
            Self::SecretStorage(v) => settings.secret_storage = *v,
            Self::Keys(v) => {
                settings.keys.map.clone_from(v);
                settings.keys.invalid.clear();
            }
        }
    }

    fn value(&self) -> Value {
        match self {
            Self::FontFamily(v) | Self::ColorScheme(v) => Value::from(v.as_str()),
            Self::FontSize(v) | Self::BackgroundOpacity(v) => Value::from(short_f64(*v)),
            Self::ScrollbackLines(v) => Value::from(i64::try_from(*v).unwrap_or(i64::MAX)),
            Self::Theme(ThemeMode::Dark) => Value::from("dark"),
            Self::Theme(ThemeMode::Light) => Value::from("light"),
            Self::KittyKeyboard(v) | Self::BackgroundBlur(v) => Value::from(*v),
            // A number for a fixed rate, the string for the display's.
            Self::MaxFps(MaxFps::Fixed(n)) => Value::from(i64::from(*n)),
            Self::MaxFps(MaxFps::Display) => Value::from("display"),
            Self::MaxFps(MaxFps::Invalid(_)) => Value::from(i64::from(DEFAULT_MAX_FPS)),
            Self::TunnelMaxRetries(v) => Value::from(i64::from(*v)),
            Self::SecretStorage(v) => Value::from(v.as_str()),
            Self::WindowDecorations(v) => Value::from(v.as_str()),
            // Written per action by `edit_keys`.
            Self::Keys(_) => Value::from(""),
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
    let loaded = toml::from_str::<Settings>(text).map_err(|e| {
        let line = e.span().map(|s| line_of(text, s.start));
        (line, e.message().to_string())
    })?;
    if let SettingChange::Keys(wanted) = change {
        // Compare with the entries as written (before load drops conflicting or reserved
        // ones), so an entry the app ignored is not left behind to apply on the next load.
        return Ok(edit_keys(text, doc, &loaded.keys.map, wanted));
    }
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

/// `text` (parsed as `doc`) with the `[keys]` table changed from `have` (what the file gives
/// now) to `wanted`. Entries of actions whose chord does not change are left alone. A missing
/// table is added at the end of the file.
fn edit_keys(text: &str, mut doc: DocumentMut, have: &Overrides, wanted: &Overrides) -> String {
    let mac = cfg!(target_os = "macos");
    let value = |chord: &Option<crate::keys::Chord>| {
        chord.map_or_else(|| "none".to_owned(), |c| c.to_config(mac))
    };
    let changed: Vec<(Action, Option<&Option<crate::keys::Chord>>)> = Action::ALL
        .into_iter()
        .filter(|a| have.get(a) != wanted.get(a))
        .map(|a| (a, wanted.get(&a)))
        .collect();
    if changed.is_empty() {
        return text.to_owned();
    }
    let Some(table) = doc.get_mut("keys").and_then(Item::as_table_like_mut) else {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut table = format!("[keys]{newline}");
        for (action, chord) in changed {
            if let Some(chord) = chord {
                let v = Value::from(value(chord));
                table.push_str(&format!(
                    "{} = {}{newline}",
                    action.name(),
                    v.to_string().trim()
                ));
            }
        }
        // Right after a commented-out `#[keys]` (as in the template) when only comments follow
        // it, else at the end.
        if let Some(after) = commented_table(text) {
            let (head, tail) = text.split_at(after);
            let sep = if head.ends_with('\n') { "" } else { newline };
            return format!("{head}{sep}{table}{tail}");
        }
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(newline);
        }
        if !out.trim().is_empty() {
            out.push_str(newline);
        }
        out.push_str(&table);
        return out;
    };
    // Comments above a removed entry, for the entry after it (or after the table).
    let mut orphaned = String::new();
    for (action, chord) in changed {
        match chord {
            None => {
                let names: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
                let comments = table
                    .get_key_value_mut(action.name())
                    .and_then(|(k, _)| {
                        k.leaf_decor()
                            .prefix()
                            .and_then(|p| p.as_str())
                            .map(str::to_owned)
                    })
                    .filter(|p| p.contains('#'));
                table.remove(action.name());
                let Some(comments) = comments else {
                    continue;
                };
                let next = names
                    .iter()
                    .skip_while(|n| *n != action.name())
                    .nth(1)
                    .cloned();
                match next.and_then(|n| table.get_key_value_mut(&n).map(|(k, _)| k)) {
                    Some(mut key) => {
                        let old = key
                            .leaf_decor()
                            .prefix()
                            .and_then(|p| p.as_str())
                            .unwrap_or("")
                            .to_owned();
                        key.leaf_decor_mut().set_prefix(format!("{comments}{old}"));
                    }
                    None => orphaned.push_str(&comments),
                }
            }
            Some(chord) => {
                let mut v = Value::from(value(chord));
                match table.get_mut(action.name()) {
                    Some(Item::Value(old)) => {
                        *v.decor_mut() = old.decor().clone();
                        *old = v;
                    }
                    _ => {
                        table.insert(action.name(), Item::Value(v));
                    }
                }
            }
        }
    }
    if !orphaned.is_empty() {
        keep_after_keys_table(&mut doc, &orphaned);
    }
    doc.to_string()
}

/// Put `comments` right after the `[keys]` table: before the next table's header, or at the
/// end of the file.
fn keep_after_keys_table(doc: &mut DocumentMut, comments: &str) {
    let keys_at = doc
        .get("keys")
        .and_then(Item::as_table)
        .and_then(toml_edit::Table::position);
    let next = keys_at.and_then(|at| {
        doc.as_table_mut()
            .iter_mut()
            .filter_map(|(_, item)| item.as_table_mut())
            .filter(|t| t.position().is_some_and(|p| p > at))
            .min_by_key(|t| t.position())
            .map(|t| {
                let old = t
                    .decor()
                    .prefix()
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_owned();
                t.decor_mut().set_prefix(format!("{comments}{old}"));
            })
    });
    if next.is_none() {
        let trailing = doc.trailing().as_str().unwrap_or("").to_owned();
        doc.set_trailing(format!("{comments}{trailing}"));
    }
}

/// The byte offset just past a `#[keys]` line, if only comments and blank lines follow it.
fn commented_table(text: &str) -> Option<usize> {
    let mut offset = 0;
    let mut found = None;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim();
        if found.is_none() {
            if trimmed.strip_prefix('#').map(str::trim) == Some("[keys]") {
                found = Some(offset);
            }
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            return None;
        }
    }
    found
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
            SettingChange::WindowDecorations(WindowDecorations::System),
            SettingChange::MaxFps(MaxFps::Fixed(120)),
            SettingChange::MaxFps(MaxFps::Display),
            SettingChange::TunnelMaxRetries(3),
            SettingChange::SecretStorage(SecretStorage::Vault),
            keys(&[(Action::Palette, "ctrl+alt+p")]),
        ]
    }

    fn chord(s: &str) -> Option<crate::keys::Chord> {
        crate::keys::Chord::parse(s).unwrap()
    }

    fn keys(entries: &[(Action, &str)]) -> SettingChange {
        SettingChange::Keys(entries.iter().map(|(a, s)| (*a, chord(s))).collect())
    }

    #[test]
    fn key_overrides_go_in_a_keys_table_under_its_comment() {
        let after = edit_text(
            SETTINGS_TEMPLATE,
            &keys(&[
                (Action::NewTab, "ctrl+shift+y"),
                (Action::OpenSettingsFile, "none"),
            ]),
        )
        .unwrap();
        assert_eq!(
            after,
            format!(
                "{SETTINGS_TEMPLATE}[keys]\nnew_tab = \"ctrl+shift+y\"\nopen_settings_file = \"none\"\n"
            )
        );
        // Without the template's `#[keys]`, the table is added after a blank line.
        let after = edit_text("theme = \"dark\"", &keys(&[(Action::Quit, "ctrl+shift+k")]));
        assert_eq!(
            after.unwrap(),
            "theme = \"dark\"\n\n[keys]\nquit = \"ctrl+shift+k\"\n"
        );
        let after = edit_text(
            SETTINGS_TEMPLATE,
            &keys(&[
                (Action::NewTab, "ctrl+shift+y"),
                (Action::OpenSettingsFile, "none"),
            ]),
        )
        .unwrap();
        let s: Settings = toml::from_str(&after).unwrap();
        assert_eq!(
            s.keys.map.get(&Action::NewTab),
            Some(&chord("ctrl+shift+y"))
        );
        assert_eq!(s.keys.map.get(&Action::OpenSettingsFile), Some(&None));
        // A top-level setting written afterwards still goes next to its comment.
        let after = edit_text(&after, &SettingChange::FontSize(15.0)).unwrap();
        assert!(
            after.contains("#font_size = 13.0\nfont_size = 15.0\n"),
            "{after}"
        );
        assert_eq!(
            toml::from_str::<Settings>(&after).unwrap().keys.map.len(),
            2
        );
    }

    #[test]
    fn key_edits_keep_comments_and_untouched_entries() {
        let before = "theme = \"dark\"\n\
                      \n\
                      # My shortcuts.\n\
                      [keys]\n\
                      # tmux owns Ctrl+B.\n\
                      new_tab = \"ctrl+shift+y\"   # muscle memory\n\
                      close_tab = \"none\"\n\
                      nope = \"ctrl+shift+u\"\n\
                      find = \"ctrl+shift+f+x\"\n";
        // Rebind new_tab, reset close_tab, add quit; the unknown and the broken entry stay.
        let after = edit_text(
            before,
            &keys(&[
                (Action::NewTab, "ctrl+shift+j"),
                (Action::Quit, "ctrl+shift+k"),
            ]),
        )
        .unwrap();
        assert_eq!(
            after,
            "theme = \"dark\"\n\
             \n\
             # My shortcuts.\n\
             [keys]\n\
             # tmux owns Ctrl+B.\n\
             new_tab = \"ctrl+shift+j\"   # muscle memory\n\
             nope = \"ctrl+shift+u\"\n\
             find = \"ctrl+shift+f+x\"\n\
             quit = \"ctrl+shift+k\"\n"
        );
        // Restoring every default removes the entries the app took, nothing else.
        let after = edit_text(&after, &keys(&[])).unwrap();
        assert_eq!(
            after,
            "theme = \"dark\"\n\
             \n\
             # My shortcuts.\n\
             [keys]\n\
             # tmux owns Ctrl+B.\n\
             nope = \"ctrl+shift+u\"\n\
             find = \"ctrl+shift+f+x\"\n"
        );
        // Nothing to change: the text stays byte for byte.
        assert_eq!(edit_text(&after, &keys(&[])).unwrap(), after);
    }

    #[test]
    fn an_entry_load_dropped_for_a_conflict_does_not_come_back() {
        // new_tab takes Close tab's chord: load drops it with a notice. Clearing Close tab in
        // the window must not leave new_tab behind to apply on the next load.
        let before = "[keys]\nnew_tab = \"ctrl+alt+shift+w\"\nclose_tab = \"ctrl+alt+shift+w\"\n";
        let mut loaded: Settings = toml::from_str(before).unwrap();
        loaded.clamp();
        let mut wanted = loaded.keys.map.clone();
        wanted.insert(Action::CloseTab, None);
        let after = edit_text(before, &SettingChange::Keys(wanted.clone())).unwrap();
        let mut reloaded: Settings = toml::from_str(&after).unwrap();
        assert!(reloaded.clamp().is_empty(), "{after}");
        assert_eq!(reloaded.keys.map, wanted, "{after}");
    }

    #[test]
    fn comments_of_removed_entries_stay() {
        let before = "[keys]\n# first\nnew_tab = \"ctrl+shift+y\"\n# last\nquit = \"ctrl+shift+k\"\n\n# Other.\n[extra]\na = 1\n";
        let after = edit_text(before, &keys(&[])).unwrap();
        assert_eq!(
            after,
            "[keys]\n# first\n# last\n\n# Other.\n[extra]\na = 1\n"
        );
        let before = "[keys]\nnew_tab = \"ctrl+shift+y\"\n# last one\nquit = \"ctrl+shift+k\"\n";
        let after = edit_text(before, &keys(&[(Action::NewTab, "ctrl+shift+y")])).unwrap();
        assert_eq!(after, "[keys]\nnew_tab = \"ctrl+shift+y\"\n# last one\n");
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
    fn secret_storage_is_written_as_its_name() {
        for (v, name) in [
            (SecretStorage::Auto, "auto"),
            (SecretStorage::Keychain, "keychain"),
            (SecretStorage::Vault, "vault"),
        ] {
            let after = edit_text(SETTINGS_TEMPLATE, &SettingChange::SecretStorage(v)).unwrap();
            assert!(
                after.contains(&format!(
                    "#secret_storage = \"auto\"\nsecret_storage = \"{name}\"\n"
                )),
                "{after}"
            );
            assert_eq!(
                toml::from_str::<Settings>(&after).unwrap().secret_storage,
                v
            );
        }
        assert_eq!(Settings::default().secret_storage, SecretStorage::Auto);
        assert!(toml::from_str::<Settings>("secret_storage = \"cloud\"\n").is_err());
    }

    #[test]
    fn window_decorations_defaults_and_writes() {
        assert_eq!(
            Settings::default().window_decorations,
            WindowDecorations::Fbterm
        );
        assert!(SETTINGS_TEMPLATE.contains(
            "# Window title bar on Linux: \"fbterm\" draws it in the tab bar, \"system\" uses \
             the desktop's.\n#window_decorations = \"fbterm\"\n"
        ));
        for (v, name) in [
            (WindowDecorations::System, "system"),
            (WindowDecorations::Fbterm, "fbterm"),
        ] {
            let change = SettingChange::WindowDecorations(v);
            assert_eq!(change.key(), "window_decorations");
            let after = edit_text(SETTINGS_TEMPLATE, &change).unwrap();
            assert!(
                after.contains(&format!(
                    "#window_decorations = \"fbterm\"\nwindow_decorations = \"{name}\"\n"
                )),
                "{after}"
            );
            assert_eq!(
                toml::from_str::<Settings>(&after)
                    .unwrap()
                    .window_decorations,
                v
            );
        }
        assert!(toml::from_str::<Settings>("window_decorations = \"none\"\n").is_err());
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
