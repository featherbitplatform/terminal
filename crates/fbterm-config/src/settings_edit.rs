//! Changing one setting in `settings.toml` in place: only that key's value changes; comments,
//! blank lines, the order of keys and every other key stay as they are.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Value};

use crate::keys::{Action, Overrides};
use crate::settings::{
    DEFAULT_MAX_FPS, MacosBlurStyle, MaxFps, SecretStorage, Settings, TabBar, UNCOMPRESSED_FLOOR,
    WindowDecorations,
};
use crate::store::{ConfigError, atomic_write, line_of};

/// One setting with its new value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingChange {
    FontFamily(String),
    FontSize(f32),
    /// Lowering it also lowers a stored `uncompressed_history_lines` that would no longer be
    /// below it (see [`UNCOMPRESSED_FLOOR`]).
    ScrollbackLines(usize),
    /// Switching it on also clamps a stored `uncompressed_history_lines` into range (it is kept
    /// as written while compression is off).
    HistoryCompression(bool),
    UncompressedHistoryLines(usize),
    ColorScheme(String),
    KittyKeyboard(bool),
    /// Show pictures programs send. Off also clears what is on screen in every open terminal.
    InlineImages(bool),
    /// Keep pictures when their rows scroll off the screen. Only counts with `inline_images` on.
    ScrollbackImages(bool),
    GraphemeClusters(bool),
    Ligatures(bool),
    BackgroundOpacity(f32),
    BackgroundBlur(bool),
    /// How blur looks on macOS.
    MacosBlurStyle(MacosBlurStyle),
    WindowDecorations(WindowDecorations),
    TabBar(TabBar),
    /// Fold the sidebar into profile groups.
    TabGroups(bool),
    /// Draw panes in their scheme's suggested font when installed.
    UseThemeFonts(bool),
    MaxFps(MaxFps),
    TunnelMaxRetries(u32),
    CheckUpdates(bool),
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
            Self::HistoryCompression(_) => "history_compression",
            Self::UncompressedHistoryLines(_) => "uncompressed_history_lines",
            Self::ColorScheme(_) => "color_scheme",
            Self::KittyKeyboard(_) => "kitty_keyboard",
            Self::InlineImages(_) => "inline_images",
            Self::ScrollbackImages(_) => "scrollback_images",
            Self::GraphemeClusters(_) => "grapheme_clusters",
            Self::Ligatures(_) => "ligatures",
            Self::BackgroundOpacity(_) => "background_opacity",
            Self::BackgroundBlur(_) => "background_blur",
            Self::MacosBlurStyle(_) => "macos_blur_style",
            Self::WindowDecorations(_) => "window_decorations",
            Self::TabBar(_) => "tab_bar",
            Self::TabGroups(_) => "tab_groups",
            Self::UseThemeFonts(_) => "use_theme_fonts",
            Self::MaxFps(_) => "max_fps",
            Self::TunnelMaxRetries(_) => "tunnel_max_retries",
            Self::CheckUpdates(_) => "check_updates",
            Self::SecretStorage(_) => "secret_storage",
            Self::Keys(_) => "keys",
        }
    }

    /// Set this value in `settings`.
    pub fn apply(&self, settings: &mut Settings) {
        match self {
            Self::FontFamily(v) => settings.font_family.clone_from(v),
            Self::FontSize(v) => settings.font_size = *v,
            Self::ScrollbackLines(v) => {
                settings.scrollback_lines = *v;
                if let Some(keep) = settings.uncompressed_history_lines {
                    settings.uncompressed_history_lines = Some(dragged_down(keep, *v));
                }
            }
            Self::HistoryCompression(v) => {
                settings.history_compression = *v;
                if *v && let Some(keep) = settings.uncompressed_history_lines {
                    settings.uncompressed_history_lines =
                        Some(clamped(keep, settings.scrollback_lines));
                }
            }
            Self::UncompressedHistoryLines(v) => settings.uncompressed_history_lines = Some(*v),
            Self::ColorScheme(v) => settings.color_scheme.clone_from(v),
            Self::KittyKeyboard(v) => settings.kitty_keyboard = *v,
            Self::InlineImages(v) => settings.inline_images = *v,
            Self::ScrollbackImages(v) => settings.scrollback_images = *v,
            Self::GraphemeClusters(v) => settings.grapheme_clusters = *v,
            Self::Ligatures(v) => settings.ligatures = *v,
            Self::BackgroundOpacity(v) => settings.background_opacity = *v,
            Self::BackgroundBlur(v) => settings.background_blur = *v,
            Self::MacosBlurStyle(v) => settings.macos_blur_style.clone_from(v),
            Self::WindowDecorations(v) => settings.window_decorations = *v,
            Self::TabBar(v) => settings.tab_bar = Some(*v),
            Self::TabGroups(v) => settings.tab_groups = *v,
            Self::UseThemeFonts(v) => settings.use_theme_fonts = *v,
            Self::MaxFps(v) => settings.max_fps.clone_from(v),
            Self::TunnelMaxRetries(v) => settings.tunnel_max_retries = *v,
            Self::CheckUpdates(v) => settings.check_updates = Some(*v),
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
            Self::ScrollbackLines(v) | Self::UncompressedHistoryLines(v) => {
                Value::from(i64::try_from(*v).unwrap_or(i64::MAX))
            }
            Self::KittyKeyboard(v)
            | Self::InlineImages(v)
            | Self::ScrollbackImages(v)
            | Self::GraphemeClusters(v)
            | Self::Ligatures(v)
            | Self::BackgroundBlur(v)
            | Self::HistoryCompression(v)
            | Self::CheckUpdates(v) => Value::from(*v),
            // A number for a fixed rate, the string for the display's.
            Self::MaxFps(MaxFps::Fixed(n)) => Value::from(i64::from(*n)),
            Self::MaxFps(MaxFps::Display) => Value::from("display"),
            Self::MaxFps(MaxFps::Invalid(_)) => Value::from(i64::from(DEFAULT_MAX_FPS)),
            Self::TunnelMaxRetries(v) => Value::from(i64::from(*v)),
            Self::SecretStorage(v) => Value::from(v.as_str()),
            Self::WindowDecorations(v) => Value::from(v.as_str()),
            Self::MacosBlurStyle(v) => Value::from(v.as_str()),
            Self::TabBar(v) => Value::from(v.as_str()),
            Self::TabGroups(v) | Self::UseThemeFonts(v) => Value::from(*v),
            // Written per action by `edit_keys`.
            Self::Keys(_) => Value::from(""),
        }
    }
}

/// `keep` (an `uncompressed_history_lines`) after `scrollback_lines` became `scrollback`:
/// unchanged unless it is no longer below it, then `scrollback - 1`. A scrollback at or below
/// [`UNCOMPRESSED_FLOOR`] turns compression off whatever the value, so it is left alone.
fn dragged_down(keep: usize, scrollback: usize) -> usize {
    if scrollback > UNCOMPRESSED_FLOOR && keep >= scrollback {
        scrollback - 1
    } else {
        keep
    }
}

/// `keep` (an `uncompressed_history_lines`) brought into its range for `scrollback`, as when
/// compression is switched on. A scrollback at or below [`UNCOMPRESSED_FLOOR`] leaves no range
/// (compression does not apply), so it is left alone.
fn clamped(keep: usize, scrollback: usize) -> usize {
    if scrollback > UNCOMPRESSED_FLOOR {
        keep.clamp(UNCOMPRESSED_FLOOR, scrollback - 1)
    } else {
        keep
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
    let edited = edit_one(text, change)?;
    // Lowering the scrollback also lowers an `uncompressed_history_lines` that is present and
    // would no longer be below it; switching compression on clamps one into range. Both read
    // the edited file, whose `scrollback_lines` is the new one. `edit_one` refused a file that
    // does not load as settings, so the edited one loads too.
    let rule: fn(usize, usize) -> usize = match change {
        SettingChange::ScrollbackLines(_) => dragged_down,
        SettingChange::HistoryCompression(true) => clamped,
        _ => return Ok(edited),
    };
    if let Ok(s) = toml::from_str::<Settings>(&edited)
        && let Some(keep) = s.uncompressed_history_lines
        && let now = rule(keep, s.scrollback_lines)
        && now != keep
    {
        return edit_one(&edited, &SettingChange::UncompressedHistoryLines(now));
    }
    Ok(edited)
}

fn edit_one(text: &str, change: &SettingChange) -> Result<String, (Option<usize>, String)> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        let line = e.span().map(|s| line_of(text, s.start));
        (line, e.message().to_string())
    })?;
    // The file must load as settings too: a value of the wrong type is as broken as bad syntax.
    let loaded = toml::from_str::<Settings>(text).map_err(|e| {
        let line = e.span().map(|s| line_of(text, s.start));
        (line, e.message().to_string())
    })?;
    // The obsolete top-level `theme` key goes with the next write (keys in tables stay).
    doc.remove("theme");
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
            SettingChange::HistoryCompression(true),
            SettingChange::UncompressedHistoryLines(5_000),
            SettingChange::KittyKeyboard(true),
            SettingChange::InlineImages(false),
            SettingChange::ScrollbackImages(true),
            SettingChange::GraphemeClusters(true),
            SettingChange::Ligatures(false),
            SettingChange::BackgroundOpacity(0.85),
            SettingChange::BackgroundBlur(true),
            SettingChange::WindowDecorations(WindowDecorations::System),
            SettingChange::TabBar(TabBar::Left),
            SettingChange::TabGroups(true),
            SettingChange::UseThemeFonts(true),
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
        let after = edit_text("ligatures = true", &keys(&[(Action::Quit, "ctrl+shift+k")]));
        assert_eq!(
            after.unwrap(),
            "ligatures = true\n\n[keys]\nquit = \"ctrl+shift+k\"\n"
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
        let before = "ligatures = true\n\
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
            "ligatures = true\n\
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
            "ligatures = true\n\
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
                      ligatures = true   # night owl\n\
                      \n\
                      # Bigger text.\n\
                      font_size = 14.0\n\
                      kitty_keyboard = false\n";
        let after = edit_text(before, &SettingChange::FontSize(16.0)).unwrap();
        assert_eq!(
            after,
            "# My settings.\n\
             ligatures = true   # night owl\n\
             \n\
             # Bigger text.\n\
             font_size = 16.0\n\
             kitty_keyboard = false\n"
        );
        let after = edit_text(before, &SettingChange::Ligatures(false)).unwrap();
        assert_eq!(
            after,
            "# My settings.\n\
             ligatures = false   # night owl\n\
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
        let after = edit_text("ligatures = true\n", &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(after, "ligatures = true\nfont_size = 15.0\n");
        // A commented key after a table header is not where a top-level key can go.
        let text = "ligatures = true\n\n[extra]\n#font_size = 13.0\n";
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
        let text = "ligatures = true\nextra = [\n#font_size = 13.0\n1]\n";
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
        std::fs::write(&real, "# mine\nligatures = true\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("settings.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        Settings::write_field(&link, &SettingChange::Ligatures(false)).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "# mine\nligatures = false\n"
        );
        let mode = std::fs::metadata(&real).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_endings_and_a_missing_final_newline_are_respected() {
        let text = "# a\r\n#font_size = 13.0\r\nligatures = true\r\n";
        let after = edit_text(text, &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(
            after,
            "# a\r\n#font_size = 13.0\r\nfont_size = 15.0\r\nligatures = true\r\n"
        );
        let after = edit_text("#font_size = 13.0", &SettingChange::FontSize(15.0)).unwrap();
        assert_eq!(after, "#font_size = 13.0\nfont_size = 15.0\n");
    }

    #[test]
    fn a_broken_file_is_refused() {
        let (line, _) = edit_text(
            "ligatures = true\nfont_size = = 3\n",
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
    fn use_theme_fonts_defaults_off_and_is_written_in_place() {
        assert!(!Settings::default().use_theme_fonts);
        let s: Settings = toml::from_str("use_theme_fonts = true\n").unwrap();
        assert!(s.use_theme_fonts);
        let sample: Settings = toml::from_str(SETTINGS_TEMPLATE).unwrap();
        assert!(
            !sample.use_theme_fonts,
            "the sample leaves it commented out"
        );
        assert!(SETTINGS_TEMPLATE.contains("#use_theme_fonts = false\n"));
        let edited = edit_text(SETTINGS_TEMPLATE, &SettingChange::UseThemeFonts(true)).unwrap();
        assert!(edited.contains("#use_theme_fonts = false\nuse_theme_fonts = true\n"));
        assert!(toml::from_str::<Settings>(&edited).unwrap().use_theme_fonts);
    }

    #[test]
    fn tab_groups_defaults_off_and_counts_only_with_tabs_left() {
        assert!(!Settings::default().tab_groups);
        assert!(!Settings::default().tab_groups_on());
        let s: Settings = toml::from_str("tab_groups = true\n").unwrap();
        assert!(s.tab_groups);
        assert!(!s.tab_groups_on(), "nothing to fold with tabs on top");
        let s: Settings = toml::from_str("tab_groups = true\ntab_bar = \"left\"\n").unwrap();
        assert!(s.tab_groups_on());
        let sample: Settings = toml::from_str(SETTINGS_TEMPLATE).unwrap();
        assert!(!sample.tab_groups, "the sample leaves it commented out");
    }

    #[test]
    fn tab_bar_is_absent_until_answered_and_parses_both_sides() {
        assert_eq!(Settings::default().tab_bar, None);
        assert!(!Settings::default().tabs_left());
        let s: Settings = toml::from_str("tab_bar = \"left\"\n").unwrap();
        assert_eq!(s.tab_bar, Some(TabBar::Left));
        assert!(s.tabs_left());
        let s: Settings = toml::from_str("tab_bar = \"top\"\n").unwrap();
        assert_eq!(s.tab_bar, Some(TabBar::Top));
        assert!(!s.tabs_left());
        assert!(toml::from_str::<Settings>("tab_bar = \"right\"\n").is_err());
        assert!(SETTINGS_TEMPLATE.contains(
            "# Where open sessions are listed: \"top\" (a row of tabs along the top of the \
             window) or\n# \"left\" (a column down the left side, scrolling when it overflows). \
             Left out, the Welcome\n# tab asks once.\n#tab_bar = \"top\"\n"
        ));
        assert_eq!(TabBar::default(), TabBar::Top);
        assert_eq!(TabBar::Left.as_str(), "left");
    }

    #[test]
    fn tab_bar_change_writes_under_its_template_line() {
        for (v, name) in [(TabBar::Left, "left"), (TabBar::Top, "top")] {
            let change = SettingChange::TabBar(v);
            assert_eq!(change.key(), "tab_bar");
            let after = edit_text(SETTINGS_TEMPLATE, &change).unwrap();
            assert!(
                after.contains(&format!("#tab_bar = \"top\"\ntab_bar = \"{name}\"\n")),
                "{after}"
            );
            let mut s = Settings::default();
            change.apply(&mut s);
            assert_eq!(s.tab_bar, Some(v));
        }
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
    fn lowering_scrollback_drags_uncompressed_down() {
        let mut s = Settings {
            uncompressed_history_lines: Some(8_000),
            ..Settings::default()
        };
        SettingChange::ScrollbackLines(5_000).apply(&mut s);
        assert_eq!(s.uncompressed_history_lines, Some(4_999));
        // Absent stays absent; a value that still fits stays.
        let mut s = Settings::default();
        SettingChange::ScrollbackLines(5_000).apply(&mut s);
        assert_eq!(s.uncompressed_history_lines, None);
        let mut s = Settings {
            uncompressed_history_lines: Some(2_000),
            ..Settings::default()
        };
        SettingChange::ScrollbackLines(5_000).apply(&mut s);
        assert_eq!(s.uncompressed_history_lines, Some(2_000));
        // At or below the floor compression is off anyway: the key is left alone.
        let mut s = Settings {
            uncompressed_history_lines: Some(8_000),
            ..Settings::default()
        };
        SettingChange::ScrollbackLines(1_000).apply(&mut s);
        assert_eq!(s.uncompressed_history_lines, Some(8_000));
        // In the file: both keys are written, only when the key is present.
        let text = "scrollback_lines = 20000\nuncompressed_history_lines = 8000\n";
        let edited = edit_text(text, &SettingChange::ScrollbackLines(5_000)).unwrap();
        assert_eq!(
            edited,
            "scrollback_lines = 5000\nuncompressed_history_lines = 4999\n"
        );
        let edited = edit_text(&edited, &SettingChange::ScrollbackLines(50_000)).unwrap();
        assert_eq!(
            edited,
            "scrollback_lines = 50000\nuncompressed_history_lines = 4999\n"
        );
        let edited = edit_text("", &SettingChange::ScrollbackLines(5_000)).unwrap();
        assert!(!edited.contains("uncompressed"), "{edited}");
    }

    /// Another key in the file is invalid: the whole edit is refused with that key's line, so
    /// the scrollback is never written without the uncompressed value it drags down (the
    /// drag-down's own read of the edited file cannot fail where this one passed).
    #[test]
    fn lowering_scrollback_beside_an_invalid_key_writes_nothing() {
        for text in [
            "tab_bar = \"purple\"\nscrollback_lines = 20000\nuncompressed_history_lines = 8000\n",
            "scrollback_lines = 20000\nuncompressed_history_lines = \"lots\"\n",
        ] {
            let (line, _) = edit_text(text, &SettingChange::ScrollbackLines(5_000)).unwrap_err();
            assert!(line.is_some(), "{text}");
        }
    }

    #[test]
    fn switching_compression_on_clamps_a_stored_uncompressed_value() {
        for (keep, want) in [(8_000, 4_999), (200, 1_000), (2_000, 2_000)] {
            let mut s = Settings {
                scrollback_lines: 5_000,
                uncompressed_history_lines: Some(keep),
                ..Settings::default()
            };
            SettingChange::HistoryCompression(true).apply(&mut s);
            assert_eq!(s.uncompressed_history_lines, Some(want), "{keep}");
        }
        // Absent stays absent; off, or a scrollback at the floor, leaves it as written.
        let mut s = Settings::default();
        SettingChange::HistoryCompression(true).apply(&mut s);
        assert_eq!(s.uncompressed_history_lines, None);
        for (scrollback, on) in [(5_000, false), (1_000, true)] {
            let mut s = Settings {
                scrollback_lines: scrollback,
                uncompressed_history_lines: Some(8_000),
                ..Settings::default()
            };
            SettingChange::HistoryCompression(on).apply(&mut s);
            assert_eq!(s.uncompressed_history_lines, Some(8_000));
        }
        // In the file: the clamped value is written with the switch, and the file then loads
        // with no problem to report.
        let text = "scrollback_lines = 5000\nuncompressed_history_lines = 8000\n";
        let edited = edit_text(text, &SettingChange::HistoryCompression(true)).unwrap();
        assert_eq!(
            edited,
            "scrollback_lines = 5000\nuncompressed_history_lines = 4999\nhistory_compression = true\n"
        );
        let mut loaded: Settings = toml::from_str(&edited).unwrap();
        assert!(loaded.clamp().is_empty());
        let edited = edit_text(text, &SettingChange::HistoryCompression(false)).unwrap();
        assert!(
            edited.contains("uncompressed_history_lines = 8000"),
            "{edited}"
        );
    }

    #[test]
    fn settings_round_trip_images() {
        // Through the text: each key goes under its own commented line, and the file loads back
        // with exactly what was set.
        let mut text = edit_text(SETTINGS_TEMPLATE, &SettingChange::InlineImages(false)).unwrap();
        text = edit_text(&text, &SettingChange::ScrollbackImages(true)).unwrap();
        assert_eq!(
            text,
            SETTINGS_TEMPLATE
                .replace(
                    "#inline_images = true\n",
                    "#inline_images = true\ninline_images = false\n"
                )
                .replace(
                    "#scrollback_images = false\n",
                    "#scrollback_images = false\nscrollback_images = true\n"
                )
        );
        let loaded: Settings = toml::from_str(&text).unwrap();
        assert!(!loaded.inline_images);
        assert!(
            loaded.scrollback_images,
            "off is the default, so it needs writing"
        );
        // Back to the defaults: both keys are written with their default value, and the file
        // still parses to what a fresh install gets.
        text = edit_text(&text, &SettingChange::InlineImages(true)).unwrap();
        text = edit_text(&text, &SettingChange::ScrollbackImages(false)).unwrap();
        assert!(text.contains("inline_images = true\n"));
        assert!(text.contains("scrollback_images = false\n"));
        let mut back = Settings::default();
        SettingChange::InlineImages(true).apply(&mut back);
        SettingChange::ScrollbackImages(false).apply(&mut back);
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), back);
        // On disk, from the template.
        let dir = std::env::temp_dir().join(format!("fbterm-edit-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.toml");
        Settings::write_field(&path, &SettingChange::ScrollbackImages(true)).unwrap();
        let s = Settings::load(&path).unwrap();
        assert!(s.inline_images, "the default: pictures show");
        assert!(s.scrollback_images);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn ligatures_written_after_its_template_line() {
        let dir = std::env::temp_dir().join(format!("fbterm-edit-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.toml");
        Settings::write_field(&path, &SettingChange::Ligatures(false)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            SETTINGS_TEMPLATE.replace(
                "#ligatures = true\n",
                "#ligatures = true\nligatures = false\n"
            )
        );
        assert!(!Settings::load(&path).unwrap().ligatures);
    }

    #[test]
    fn grapheme_clusters_written_after_its_template_line() {
        let dir = std::env::temp_dir().join(format!("fbterm-edit-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.toml");
        Settings::write_field(&path, &SettingChange::GraphemeClusters(true)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            SETTINGS_TEMPLATE.replace(
                "#grapheme_clusters = false\n",
                "#grapheme_clusters = false\ngrapheme_clusters = true\n"
            )
        );
        assert!(Settings::load(&path).unwrap().grapheme_clusters);
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
