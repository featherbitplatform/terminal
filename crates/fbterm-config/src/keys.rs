//! Keyboard shortcuts as data: the application actions, chords, the platform defaults, the
//! user's overrides (the `[keys]` table of `settings.toml`) and the rules for them (which chords
//! may be bound, which ones programs in the terminal would miss).
//!
//! In the file a chord is written as lowercase modifiers and a key joined by `+`, modifiers in
//! the order `ctrl`, `alt`, `shift`, `cmd`: `"cmd+t"`, `"ctrl+shift+t"`, `"ctrl+alt+f"`,
//! `"f5"`. `"none"` leaves an action without a chord. Only overrides are stored; the defaults
//! live here, per platform.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// What an application shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    /// Select tab `n` (1-based, 1..=9).
    SelectTab(u8),
    Palette,
    NewWindow,
    /// Save the layout and exit: Cmd+Q on macOS, Ctrl+Shift+Q elsewhere.
    Quit,
    /// The profile editor: Cmd+Shift+, on macOS, Ctrl+Shift+, elsewhere.
    EditProfiles,
    /// Copy a profile (the one selected in the profile editor, or the focused pane's) and
    /// open the copy in the editor (no chord by default).
    DuplicateProfile,
    /// The Settings window: Cmd+, on macOS, Ctrl+, elsewhere.
    Settings,
    /// The Color schemes window (no chord by default).
    EditSchemes,
    /// The Tunnels window (no chord by default).
    Tunnels,
    /// The archive window in its Export mode (no chord by default).
    ExportProfiles,
    /// The archive window in its Import mode (no chord by default).
    ImportProfiles,
    /// Show or hide the file panel of the active tab: Cmd+Shift+E on macOS, Ctrl+Shift+E
    /// elsewhere.
    FilePanel,
    /// Create `settings.toml` if missing and open it with the system's default app (no chord
    /// by default).
    OpenSettingsFile,
    /// Show the config folder in the file manager (no chord by default).
    RevealConfigFolder,
    /// Save the active tab's text to a file (no chord by default).
    ExportTab,
    /// The active tab's find bar: Cmd+F on macOS, Ctrl+Shift+F elsewhere.
    Find,
    /// The search over every tab: Cmd+Shift+F on macOS, Ctrl+Alt+F elsewhere.
    FindAll,
    /// Next match up the scrollback (older), only while the find bar is open: Cmd+G on macOS,
    /// Ctrl+G elsewhere.
    FindNext,
    /// Next match down the scrollback (newer), only while the find bar is open: Cmd+Shift+G on
    /// macOS, Ctrl+Shift+G elsewhere.
    FindPrevious,
    /// Split the focused pane, the new pane to its right: Cmd+D on macOS, Ctrl+Shift+D elsewhere.
    SplitRight,
    /// Split the focused pane, the new pane below it: Cmd+Shift+D on macOS, Ctrl+Shift+Alt+D
    /// elsewhere.
    SplitDown,
    /// Move focus to the neighbouring pane: Cmd+Option+arrow on macOS, Ctrl+Alt+arrow elsewhere.
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    /// Bigger text in the focused pane only: Cmd+= on macOS, Ctrl+Shift+= elsewhere.
    ZoomIn,
    /// Smaller text in the focused pane only: Cmd+- on macOS, Ctrl+Shift+- elsewhere.
    ZoomOut,
    /// The focused pane's text back to the Settings size: Cmd+0 on macOS, Ctrl+Shift+0 elsewhere.
    ZoomReset,
    /// Let the focused pane fill the tab, or give the tab back to all panes: Cmd+Shift+Enter on
    /// macOS, Ctrl+Shift+Enter elsewhere.
    MaximizePane,
    /// Search the panes of the active tab (no chord by default).
    FindInTab,
    /// Move this tab's panes into another tab (no chord by default).
    MergeTab,
    /// Move the focused pane elsewhere (no chord by default).
    MoveTile,
    /// Move the focused pane into a tab of its own (no chord by default).
    TileToTab,
}

/// The groups the Settings window lists actions in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionGroup {
    Tabs,
    Panes,
    Windows,
    Search,
    App,
}

impl ActionGroup {
    pub const ALL: [ActionGroup; 5] = [
        Self::Tabs,
        Self::Panes,
        Self::Windows,
        Self::Search,
        Self::App,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Tabs => "Tabs",
            Self::Panes => "Panes",
            Self::Windows => "Windows",
            Self::Search => "Search",
            Self::App => "App",
        }
    }
}

impl Action {
    /// Every action, in the order the Settings window lists them.
    pub const ALL: [Action; 45] = [
        Action::NewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PrevTab,
        Action::SelectTab(1),
        Action::SelectTab(2),
        Action::SelectTab(3),
        Action::SelectTab(4),
        Action::SelectTab(5),
        Action::SelectTab(6),
        Action::SelectTab(7),
        Action::SelectTab(8),
        Action::SelectTab(9),
        Action::ExportTab,
        Action::MergeTab,
        Action::SplitRight,
        Action::SplitDown,
        Action::FocusLeft,
        Action::FocusRight,
        Action::FocusUp,
        Action::FocusDown,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomReset,
        Action::MaximizePane,
        Action::MoveTile,
        Action::TileToTab,
        Action::NewWindow,
        Action::EditProfiles,
        Action::DuplicateProfile,
        Action::ExportProfiles,
        Action::ImportProfiles,
        Action::Settings,
        Action::EditSchemes,
        Action::Tunnels,
        Action::FilePanel,
        Action::Find,
        Action::FindAll,
        Action::FindNext,
        Action::FindPrevious,
        Action::FindInTab,
        Action::Palette,
        Action::OpenSettingsFile,
        Action::RevealConfigFolder,
        Action::Quit,
    ];

    /// The action's name in the `[keys]` table.
    pub fn name(self) -> &'static str {
        match self {
            Self::NewTab => "new_tab",
            Self::CloseTab => "close_tab",
            Self::NextTab => "next_tab",
            Self::PrevTab => "previous_tab",
            Self::SelectTab(1) => "select_tab_1",
            Self::SelectTab(2) => "select_tab_2",
            Self::SelectTab(3) => "select_tab_3",
            Self::SelectTab(4) => "select_tab_4",
            Self::SelectTab(5) => "select_tab_5",
            Self::SelectTab(6) => "select_tab_6",
            Self::SelectTab(7) => "select_tab_7",
            Self::SelectTab(8) => "select_tab_8",
            Self::SelectTab(_) => "select_tab_9",
            Self::Palette => "palette",
            Self::NewWindow => "new_window",
            Self::Quit => "quit",
            Self::EditProfiles => "edit_profiles",
            Self::DuplicateProfile => "duplicate_profile",
            Self::Settings => "settings",
            Self::EditSchemes => "edit_schemes",
            Self::Tunnels => "tunnels",
            Self::ExportProfiles => "export_profiles",
            Self::ImportProfiles => "import_profiles",
            Self::FilePanel => "file_panel",
            Self::OpenSettingsFile => "open_settings_file",
            Self::RevealConfigFolder => "reveal_config_folder",
            Self::ExportTab => "export_tab",
            Self::Find => "find",
            Self::FindAll => "find_all",
            Self::FindNext => "find_next",
            Self::FindPrevious => "find_previous",
            Self::SplitRight => "split_right",
            Self::SplitDown => "split_down",
            Self::FocusLeft => "focus_left",
            Self::FocusRight => "focus_right",
            Self::FocusUp => "focus_up",
            Self::FocusDown => "focus_down",
            Self::ZoomIn => "zoom_in",
            Self::ZoomOut => "zoom_out",
            Self::ZoomReset => "zoom_reset",
            Self::MaximizePane => "maximize_pane",
            Self::FindInTab => "find_in_tab",
            Self::MergeTab => "merge_tab",
            Self::MoveTile => "move_tile",
            Self::TileToTab => "tile_to_tab",
        }
    }

    /// The action named `name` in the `[keys]` table.
    pub fn from_name(name: &str) -> Option<Action> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    /// What the action is called in the app.
    pub fn title(self) -> String {
        match self {
            Self::NewTab => "New tab".into(),
            Self::CloseTab => "Close pane".into(),
            Self::NextTab => "Next tab".into(),
            Self::PrevTab => "Previous tab".into(),
            Self::SelectTab(n) => format!("Tab {n}"),
            Self::Palette => "Command palette".into(),
            Self::NewWindow => "New window".into(),
            Self::Quit => "Quit".into(),
            Self::EditProfiles => "Edit profiles".into(),
            Self::DuplicateProfile => "Duplicate profile".into(),
            Self::Settings => "Settings".into(),
            Self::EditSchemes => "Color schemes".into(),
            Self::Tunnels => "Tunnels".into(),
            Self::ExportProfiles => "Export profiles\u{2026}".into(),
            Self::ImportProfiles => "Import profiles\u{2026}".into(),
            Self::FilePanel => "File panel".into(),
            Self::OpenSettingsFile => "Open settings file".into(),
            Self::RevealConfigFolder => "Reveal config folder".into(),
            Self::ExportTab => "Export tab to file".into(),
            Self::Find => "Find in pane".into(),
            Self::FindAll => "Find in all tabs".into(),
            Self::FindNext => "Older match".into(),
            Self::FindPrevious => "Newer match".into(),
            Self::SplitRight => "Split right".into(),
            Self::SplitDown => "Split down".into(),
            Self::FocusLeft => "Focus left".into(),
            Self::FocusRight => "Focus right".into(),
            Self::FocusUp => "Focus up".into(),
            Self::FocusDown => "Focus down".into(),
            Self::ZoomIn => "Zoom in".into(),
            Self::ZoomOut => "Zoom out".into(),
            Self::ZoomReset => "Reset zoom".into(),
            Self::MaximizePane => "Maximize pane".into(),
            Self::FindInTab => "Find in this tab".into(),
            Self::MergeTab => "Merge tab into\u{2026}".into(),
            Self::MoveTile => "Move tile\u{2026}".into(),
            Self::TileToTab => "Move tile to new tab".into(),
        }
    }

    pub fn group(self) -> ActionGroup {
        match self {
            Self::NewTab
            | Self::CloseTab
            | Self::NextTab
            | Self::PrevTab
            | Self::SelectTab(_)
            | Self::ExportTab
            | Self::MergeTab => ActionGroup::Tabs,
            Self::SplitRight
            | Self::SplitDown
            | Self::FocusLeft
            | Self::FocusRight
            | Self::FocusUp
            | Self::FocusDown
            | Self::ZoomIn
            | Self::ZoomOut
            | Self::ZoomReset
            | Self::MaximizePane
            | Self::MoveTile
            | Self::TileToTab => ActionGroup::Panes,
            Self::NewWindow
            | Self::EditProfiles
            | Self::DuplicateProfile
            | Self::Settings
            | Self::EditSchemes
            | Self::Tunnels
            | Self::ExportProfiles
            | Self::ImportProfiles
            | Self::FilePanel => ActionGroup::Windows,
            Self::Find | Self::FindAll | Self::FindNext | Self::FindPrevious | Self::FindInTab => {
                ActionGroup::Search
            }
            Self::Palette | Self::OpenSettingsFile | Self::RevealConfigFolder | Self::Quit => {
                ActionGroup::App
            }
        }
    }

    /// Whether the action's chord cannot be changed on this platform: on macOS Quit stays Cmd+Q,
    /// the key every Mac app quits with (the app menu's Quit handles it, except while file
    /// transfers run: then the app takes it from the menu so this binding can ask first).
    pub fn is_fixed(self, mac: bool) -> bool {
        mac && self == Self::Quit
    }

    /// Moving between find-bar matches: these chords act only while the find bar is open, so
    /// at other times the key still reaches the terminal.
    pub fn is_find_navigation(self) -> bool {
        matches!(self, Self::FindNext | Self::FindPrevious)
    }
}

/// The key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordKey {
    /// A character key, lowercase for letters.
    Char(char),
    /// F1 to F24.
    F(u8),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    Delete,
    Backspace,
    Space,
    Tab,
    Enter,
    Escape,
}

const NAMED: [(ChordKey, &str); 15] = [
    (ChordKey::Up, "up"),
    (ChordKey::Down, "down"),
    (ChordKey::Left, "left"),
    (ChordKey::Right, "right"),
    (ChordKey::PageUp, "pageup"),
    (ChordKey::PageDown, "pagedown"),
    (ChordKey::Home, "home"),
    (ChordKey::End, "end"),
    (ChordKey::Insert, "insert"),
    (ChordKey::Delete, "delete"),
    (ChordKey::Backspace, "backspace"),
    (ChordKey::Space, "space"),
    (ChordKey::Tab, "tab"),
    (ChordKey::Enter, "enter"),
    (ChordKey::Escape, "escape"),
];

/// The symbols a press with Shift held may report instead of their key on some platforms:
/// `}` for `]`, `{` for `[` and `<` for `,`. Only these fold, as they always have: the key a
/// press reports is the layout's own unshifted key, so folding other symbols would make, for
/// example, `&` (the 1 key on AZERTY) count as `7`. (`+` and `=` are one key for shortcuts, see
/// [`Chord::same_plus_key`].)
const SHIFTED: [(char, char); 3] = [('{', '['), ('}', ']'), ('<', ',')];

impl ChordKey {
    /// A character key, lowercase; `None` for whitespace and control characters.
    pub fn from_char(c: char) -> Option<Self> {
        if c == ' ' {
            return Some(Self::Space);
        }
        if c.is_control() || c.is_whitespace() {
            return None;
        }
        let mut lower = c.to_lowercase();
        let l = lower.next()?;
        Some(Self::Char(if lower.next().is_none() { l } else { c }))
    }

    /// The key as written in `settings.toml`.
    fn config_name(self) -> String {
        match self {
            Self::Char('+') => "plus".into(),
            Self::Char(c) => c.to_string(),
            Self::F(n) => format!("f{n}"),
            named => NAMED
                .iter()
                .find(|(k, _)| *k == named)
                .map_or_else(String::new, |(_, n)| (*n).to_owned()),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        if s == "plus" {
            return Some(Self::Char('+'));
        }
        if let Some((k, _)) = NAMED.iter().find(|(_, n)| *n == s) {
            return Some(*k);
        }
        if let Some(n) = s.strip_prefix('f').and_then(|d| d.parse::<u8>().ok())
            && (1..=24).contains(&n)
        {
            return Some(Self::F(n));
        }
        let mut chars = s.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        Self::from_char(c)
    }

    fn label(self, mac: bool) -> String {
        match self {
            Self::Char(c) => c.to_uppercase().collect(),
            Self::F(n) => format!("F{n}"),
            Self::Up => "↑".into(),
            Self::Down => "↓".into(),
            Self::Left => "←".into(),
            Self::Right => "→".into(),
            Self::PageUp => "PageUp".into(),
            Self::PageDown => "PageDown".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::Insert => "Insert".into(),
            Self::Delete if mac => "⌦".into(),
            Self::Delete => "Delete".into(),
            Self::Backspace if mac => "⌫".into(),
            Self::Backspace => "Backspace".into(),
            Self::Space => "Space".into(),
            Self::Tab if mac => "⇥".into(),
            Self::Tab => "Tab".into(),
            Self::Enter if mac => "↩".into(),
            Self::Enter => "Enter".into(),
            Self::Escape => "Esc".into(),
        }
    }
}

/// A chord: a key plus the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: ChordKey,
    pub ctrl: bool,
    /// Option on macOS.
    pub alt: bool,
    pub shift: bool,
    /// Cmd on macOS, Super (the Windows key) elsewhere.
    pub cmd: bool,
}

impl Chord {
    /// `key` with no modifiers.
    pub const fn key(key: ChordKey) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
        }
    }

    /// A pressed key, with Shift folded in for the few symbols a platform may report shifted
    /// (`}` with Shift is `]`, see [`SHIFTED`]), so those chords do not depend on it.
    pub fn pressed(key: ChordKey, ctrl: bool, alt: bool, shift: bool, cmd: bool) -> Self {
        let key = match key {
            ChordKey::Char(c) if shift => SHIFTED
                .iter()
                .find(|(s, _)| *s == c)
                .map_or(key, |(_, base)| ChordKey::Char(*base)),
            other => other,
        };
        Self {
            key,
            ctrl,
            alt,
            shift,
            cmd,
        }
    }

    /// Whether `self` and `other` are the plus key, however it is typed: `=` with Shift (US
    /// layouts), `=` alone, or a layout's own `+` key, with the same Ctrl and Alt. With Cmd the
    /// Shift state does not matter (Cmd+= and Cmd+plus both zoom in); without it a `+` or a
    /// Shift+`=` stand together and a bare `=` is only itself, so Ctrl+= keeps reaching programs.
    pub fn same_plus_key(self, other: Chord) -> bool {
        let plus = |c: Chord| matches!(c.key, ChordKey::Char('+' | '='));
        let shifted = |c: Chord| c.shift || c.key == ChordKey::Char('+');
        plus(self)
            && plus(other)
            && (self.ctrl, self.alt, self.cmd) == (other.ctrl, other.alt, other.cmd)
            && (self.cmd || shifted(self) == shifted(other))
    }

    /// Read a chord as written in `settings.toml`: `Ok(None)` for `"none"` (unbound).
    /// Case and spaces around the parts do not matter; `control`, `option`/`opt`, and
    /// `command`/`super`/`win` are accepted for `ctrl`, `alt` and `cmd`.
    pub fn parse(s: &str) -> Result<Option<Chord>, String> {
        let lower = s.trim().to_lowercase();
        if lower == "none" {
            return Ok(None);
        }
        if lower.is_empty() {
            return Err("is empty".into());
        }
        let parts: Vec<&str> = lower.split('+').map(str::trim).collect();
        let Some((key, mods)) = parts.split_last() else {
            return Err("is empty".into());
        };
        let (mut ctrl, mut alt, mut shift, mut cmd) = (false, false, false, false);
        for m in mods {
            match *m {
                "ctrl" | "control" => ctrl = true,
                "alt" | "opt" | "option" => alt = true,
                "shift" => shift = true,
                "cmd" | "command" | "super" | "win" => cmd = true,
                "" => return Err("has an empty part (write + as \"plus\")".into()),
                other => return Err(format!("has an unknown modifier {other:?}")),
            }
        }
        if key.is_empty() {
            return Err("has no key (write + as \"plus\")".into());
        }
        let Some(key) = ChordKey::parse(key) else {
            return Err(format!("has an unknown key {key:?}"));
        };
        Ok(Some(Self::pressed(key, ctrl, alt, shift, cmd)))
    }

    /// The chord as written in `settings.toml`: `cmd` is written `super` off macOS.
    pub fn to_config(self, mac: bool) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("ctrl".into());
        }
        if self.alt {
            parts.push("alt".into());
        }
        if self.shift {
            parts.push("shift".into());
        }
        if self.cmd {
            parts.push(if mac { "cmd" } else { "super" }.into());
        }
        parts.push(self.key.config_name());
        parts.join("+")
    }

    /// How the chord is shown: `⇧⌘,` on macOS, `Ctrl+Shift+T` elsewhere.
    pub fn label(self, mac: bool) -> String {
        let key = self.key.label(mac);
        if mac {
            let mut s = String::new();
            if self.ctrl {
                s.push('⌃');
            }
            if self.alt {
                s.push('⌥');
            }
            if self.shift {
                s.push('⇧');
            }
            if self.cmd {
                s.push('⌘');
            }
            s.push_str(&key);
            s
        } else {
            let mut parts: Vec<String> = Vec::new();
            if self.ctrl {
                parts.push("Ctrl".into());
            }
            if self.alt {
                parts.push("Alt".into());
            }
            if self.shift {
                parts.push("Shift".into());
            }
            if self.cmd {
                parts.push("Super".into());
            }
            parts.push(key);
            parts.join("+")
        }
    }

    /// Whether the chord may be bound: it needs Ctrl, Alt or Cmd (Super), except F1 to F12,
    /// and Escape is never a shortcut (it cancels recording, closes bars and dialogs).
    pub fn allowed(self) -> Result<(), &'static str> {
        if self.key == ChordKey::Escape {
            return Err("Escape cannot be a shortcut");
        }
        if self.ctrl || self.alt || self.cmd {
            return Ok(());
        }
        match self.key {
            ChordKey::F(n) if n <= 12 => Ok(()),
            _ => Err("needs Ctrl, Alt or Cmd (only F1 to F12 work alone)"),
        }
    }
}

impl fmt::Display for Chord {
    /// The `settings.toml` form for this platform.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_config(cfg!(target_os = "macos")))
    }
}

fn mac_chord(key: char, shift: bool) -> Chord {
    Chord {
        key: ChordKey::Char(key),
        ctrl: false,
        alt: false,
        shift,
        cmd: true,
    }
}

fn ctrl_chord(key: char, shift: bool, alt: bool) -> Chord {
    Chord {
        key: ChordKey::Char(key),
        ctrl: true,
        alt,
        shift,
        cmd: false,
    }
}

/// Directional pane focus: Cmd+Option+arrow on macOS, Ctrl+Alt+arrow elsewhere.
fn arrow_chord(mac: bool, key: ChordKey) -> Chord {
    Chord {
        key,
        ctrl: !mac,
        alt: true,
        shift: false,
        cmd: mac,
    }
}

/// What already owns `chord` outside the binding table, so it cannot be bound: the terminal's
/// copy, paste and scroll keys, Save in the profile editor, and on macOS Quit (fixed on Cmd+Q)
/// and the app menu's Hide and Hide Others (which act before any window sees the key).
pub fn reserved(mac: bool, chord: Chord) -> Option<&'static str> {
    let Chord {
        key,
        ctrl,
        alt,
        shift,
        cmd,
    } = chord;
    let ch = |c: char| key == ChordKey::Char(c);
    // Copy and paste: Cmd (Shift optional) on macOS, Ctrl+Shift (Super optional) elsewhere.
    let clipboard = if mac {
        cmd && !ctrl && !alt
    } else {
        ctrl && shift && !alt
    };
    if clipboard && ch('c') {
        return Some("Copy");
    }
    if clipboard && ch('v') {
        return Some("Paste");
    }
    let scroll_key = matches!(
        key,
        ChordKey::PageUp | ChordKey::PageDown | ChordKey::Home | ChordKey::End
    );
    let shift_only = shift && !ctrl && !alt && !cmd;
    let cmd_only = cmd && !shift && !ctrl && !alt;
    if scroll_key && shift_only || mac && cmd_only && matches!(key, ChordKey::Home | ChordKey::End)
    {
        return Some("Scroll");
    }
    let save = if mac {
        cmd_only
    } else {
        ctrl && !shift && !alt && !cmd
    };
    if save && ch('s') {
        return Some("Save in the profile editor");
    }
    if mac && cmd_only && ch('q') {
        return Some("Quit (system)");
    }
    if mac && cmd_only && ch('h') {
        return Some("Hide (system)");
    }
    if mac && cmd && alt && !ctrl && !shift && ch('h') {
        return Some("Hide Others (system)");
    }
    None
}

/// A shortcut outside the binding table, shown read-only in the Settings window: its chord is
/// one [`reserved`] keeps, so it can neither be changed nor taken by an action. (Quit on macOS
/// is an action of its own, listed with the others as fixed, see [`Action::is_fixed`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedShortcut {
    pub title: &'static str,
    pub chord: Chord,
    /// A second chord doing the same.
    pub also: Option<Chord>,
    /// A line under the row.
    pub note: Option<&'static str>,
}

/// The fixed shortcuts on macOS (`mac`) or the other platforms, in the order shown.
pub fn fixed_shortcuts(mac: bool) -> Vec<FixedShortcut> {
    let key = |key: ChordKey, shift: bool, cmd: bool| Chord {
        key,
        ctrl: false,
        alt: false,
        shift,
        cmd,
    };
    let clipboard = |c: char| {
        if mac {
            mac_chord(c, false)
        } else {
            ctrl_chord(c, true, false)
        }
    };
    let save = if mac {
        mac_chord('s', false)
    } else {
        ctrl_chord('s', false, false)
    };
    let row = |title, chord| FixedShortcut {
        title,
        chord,
        also: None,
        note: None,
    };
    let mut list = vec![
        FixedShortcut {
            note: (!mac).then_some("Ctrl+C goes to the program. Right-click copies a selection."),
            ..row("Copy", clipboard('c'))
        },
        FixedShortcut {
            note: (!mac).then_some("Right-click with nothing selected offers Paste."),
            ..row("Paste", clipboard('v'))
        },
        row("Scroll up a page", key(ChordKey::PageUp, true, false)),
        row("Scroll down a page", key(ChordKey::PageDown, true, false)),
        FixedShortcut {
            also: mac.then(|| key(ChordKey::Home, false, true)),
            ..row("Scroll to top", key(ChordKey::Home, true, false))
        },
        FixedShortcut {
            also: mac.then(|| key(ChordKey::End, false, true)),
            ..row("Scroll to bottom", key(ChordKey::End, true, false))
        },
        FixedShortcut {
            note: Some("In the profile editor and the Color schemes window."),
            ..row("Save", save)
        },
    ];
    if mac {
        list.push(row("Hide fbterm", mac_chord('h', false)));
        list.push(row(
            "Hide others",
            Chord {
                alt: true,
                ..mac_chord('h', false)
            },
        ));
    }
    list
}

/// The built-in chord for `action` on macOS (`mac`) or the other platforms, if it has one.
///
/// macOS chords use Cmd (with Shift where noted) and never Ctrl; Option only with the arrows
/// that move pane focus. Elsewhere chords
/// are Ctrl+Shift, so plain Ctrl keys keep reaching the terminal; the exceptions are Ctrl+,
/// for Settings (the comma has no control code of its own), Ctrl+Alt+F for find in all tabs,
/// Ctrl+Shift+Alt+D for split down, Ctrl+Alt+arrow for pane focus, and Ctrl+G / Ctrl+Shift+G,
/// which only act while the find bar is open.
pub fn default_chord(mac: bool, action: Action) -> Option<Chord> {
    let (key, mac_shift) = match action {
        Action::NewTab => ('t', false),
        Action::CloseTab => ('w', false),
        Action::NewWindow => ('n', false),
        Action::Quit => ('q', false),
        Action::NextTab => (']', true),
        Action::PrevTab => ('[', true),
        Action::Palette => ('p', true),
        Action::FilePanel => ('e', true),
        Action::SelectTab(n @ 1..=9) => (char::from(b'0' + n), false),
        Action::EditProfiles => (',', true),
        Action::Settings if !mac => return Some(ctrl_chord(',', false, false)),
        Action::Settings => (',', false),
        Action::Find => ('f', false),
        Action::FindAll if !mac => return Some(ctrl_chord('f', false, true)),
        Action::FindAll => ('f', true),
        Action::FindNext | Action::FindPrevious => {
            let shift = action == Action::FindPrevious;
            return Some(if mac {
                mac_chord('g', shift)
            } else {
                ctrl_chord('g', shift, false)
            });
        }
        Action::SplitRight => ('d', false),
        Action::SplitDown if !mac => return Some(ctrl_chord('d', true, true)),
        Action::SplitDown => ('d', true),
        Action::FocusLeft => return Some(arrow_chord(mac, ChordKey::Left)),
        Action::FocusRight => return Some(arrow_chord(mac, ChordKey::Right)),
        Action::FocusUp => return Some(arrow_chord(mac, ChordKey::Up)),
        Action::FocusDown => return Some(arrow_chord(mac, ChordKey::Down)),
        Action::ZoomIn => ('=', false),
        Action::ZoomOut => ('-', false),
        Action::ZoomReset => ('0', false),
        Action::MaximizePane => {
            return Some(Chord {
                key: ChordKey::Enter,
                ctrl: !mac,
                alt: false,
                shift: true,
                cmd: mac,
            });
        }
        Action::SelectTab(_)
        | Action::OpenSettingsFile
        | Action::RevealConfigFolder
        | Action::EditSchemes
        | Action::Tunnels
        | Action::ExportProfiles
        | Action::ImportProfiles
        | Action::DuplicateProfile
        | Action::ExportTab
        | Action::FindInTab
        | Action::MergeTab
        | Action::MoveTile
        | Action::TileToTab => {
            return None;
        }
    };
    Some(if mac {
        mac_chord(key, mac_shift)
    } else {
        ctrl_chord(key, true, false)
    })
}

/// The user's changes to the default chords: an action mapped to `Some(chord)` or to `None`
/// (unbound). Actions not listed keep their default.
pub type Overrides = BTreeMap<Action, Option<Chord>>;

/// The `[keys]` table of `settings.toml`. Loading is lenient: entries that do not name an
/// action or a chord that may be bound are kept aside as problems for
/// [`crate::Settings::clamp`] to report, and those actions keep their default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyOverrides {
    pub map: Overrides,
    /// `(name as written, what is wrong)` for entries not taken.
    pub invalid: Vec<(String, String)>,
}

impl KeyOverrides {
    /// Drop overrides of actions that cannot change ([`Action::is_fixed`]) and of chords
    /// something else owns ([`reserved`]). Returns `(action, why)` for each override dropped.
    pub fn drop_reserved(&mut self, mac: bool) -> Vec<(Action, String)> {
        let mut dropped = Vec::new();
        self.map.retain(|action, chord| {
            if action.is_fixed(mac) {
                dropped.push((*action, "cannot be changed on macOS".to_owned()));
                return false;
            }
            match chord.and_then(|c| reserved(mac, c).map(|what| (c, what))) {
                Some((c, what)) => {
                    dropped.push((*action, format!("{:?} is used by {what}", c.to_config(mac))));
                    false
                }
                None => true,
            }
        });
        dropped
    }

    /// Drop overrides that give a chord to two actions. Each overridden action that shares its
    /// chord falls back to its default, until no chord is shared (the defaults never share
    /// one). Returns `(action, chord, the other action)` for each override dropped.
    pub fn resolve_conflicts(&mut self, mac: bool) -> Vec<(Action, Chord, Action)> {
        let mut dropped = Vec::new();
        loop {
            let bindings = Bindings::new(mac, &self.map);
            let clash = self.map.iter().find_map(|(action, chord)| {
                let chord = (*chord)?;
                let other = Action::ALL
                    .into_iter()
                    .find(|a| a != action && bindings.chord(*a) == Some(chord))?;
                Some((*action, chord, other))
            });
            let Some((action, chord, other)) = clash else {
                return dropped;
            };
            self.map.remove(&action);
            dropped.push((action, chord, other));
        }
    }
}

impl Serialize for KeyOverrides {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mac = cfg!(target_os = "macos");
        let mut m = s.serialize_map(Some(self.map.len()))?;
        for (action, chord) in &self.map {
            let value = chord.map_or_else(|| "none".to_owned(), |c| c.to_config(mac));
            m.serialize_entry(action.name(), &value)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for KeyOverrides {
    /// Lenient: any table loads; what is not a known action with a chord that may be bound is
    /// listed in [`KeyOverrides::invalid`] (a value that is not a table is still a type error).
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Other(toml::Value),
        }
        let raw = BTreeMap::<String, Raw>::deserialize(d)?;
        let mut out = Self::default();
        for (name, value) in raw {
            let Some(action) = Action::from_name(&name) else {
                out.invalid
                    .push((name, "is not an action, ignored".to_owned()));
                continue;
            };
            let text = match value {
                Raw::Str(s) => s,
                Raw::Other(v) => {
                    out.invalid.push((
                        name,
                        format!("{v} is not a chord in quotes, using the default"),
                    ));
                    continue;
                }
            };
            match Chord::parse(&text) {
                Ok(Some(chord)) => match chord.allowed() {
                    Ok(()) => {
                        out.map.insert(action, Some(chord));
                    }
                    Err(why) => out
                        .invalid
                        .push((name, format!("{text:?}: {why}, using the default"))),
                },
                Ok(None) => {
                    out.map.insert(action, None);
                }
                Err(why) => out
                    .invalid
                    .push((name, format!("{text:?} {why}, using the default"))),
            }
        }
        Ok(out)
    }
}

/// The chord of every action: the platform defaults with the overrides applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Bindings {
    mac: bool,
    overrides: Overrides,
}

impl Bindings {
    /// The bindings for macOS (`mac`) or the other platforms with `overrides` applied.
    pub fn new(mac: bool, overrides: &Overrides) -> Self {
        Self {
            mac,
            overrides: overrides.clone(),
        }
    }

    /// The built-in bindings.
    pub fn defaults(mac: bool) -> Self {
        Self::new(mac, &Overrides::new())
    }

    /// The bindings of this platform with `overrides` applied.
    pub fn current(overrides: &Overrides) -> Self {
        Self::new(cfg!(target_os = "macos"), overrides)
    }

    pub fn mac(&self) -> bool {
        self.mac
    }

    pub fn overrides(&self) -> &Overrides {
        &self.overrides
    }

    /// The chord that triggers `action`, if it has one.
    pub fn chord(&self, action: Action) -> Option<Chord> {
        match self.overrides.get(&action) {
            Some(chord) => *chord,
            None => default_chord(self.mac, action),
        }
    }

    /// The action `chord` triggers, if any.
    ///
    /// The plus key matches however it is typed (see [`Chord::same_plus_key`]), for whichever
    /// action has a plus chord, so a rebound zoom keeps that.
    pub fn action(&self, chord: Chord) -> Option<Action> {
        let exact = Action::ALL
            .into_iter()
            .find(|a| self.chord(*a) == Some(chord));
        exact.or_else(|| {
            Action::ALL
                .into_iter()
                .find(|a| self.chord(*a).is_some_and(|c| c.same_plus_key(chord)))
        })
    }

    /// Whether `action` has its default chord.
    pub fn is_default(&self, action: Action) -> bool {
        self.chord(action) == default_chord(self.mac, action)
    }

    /// The chord's label for `action`, if it has a chord.
    pub fn label(&self, action: Action) -> Option<String> {
        self.chord(action).map(|c| c.label(self.mac))
    }
}

/// `overrides` with `action` set to `chord` (`None`: unbound): an override equal to the
/// default is dropped, so only real changes are stored.
pub fn set_override(mac: bool, overrides: &mut Overrides, action: Action, chord: Option<Chord>) {
    if default_chord(mac, action) == chord {
        overrides.remove(&action);
    } else {
        overrides.insert(action, chord);
    }
}

/// What programs in the terminal use well-known Ctrl chords for (Ctrl plus the key, no other
/// modifier).
const CTRL_USES: [(char, &str); 27] = [
    (
        'a',
        "moves to the start of the line in bash and zsh, and is a common tmux prefix",
    ),
    (
        'b',
        "is the default tmux prefix, and moves back a character in bash and zsh",
    ),
    ('c', "interrupts the running program"),
    ('d', "ends input, which exits a shell"),
    ('e', "moves to the end of the line in bash and zsh"),
    (
        'f',
        "moves forward a character in bash and zsh, and scrolls a page in vim",
    ),
    ('g', "cancels a command in emacs"),
    ('h', "is Backspace for many programs"),
    ('i', "is Tab for programs in the terminal"),
    ('j', "is a new line for programs in the terminal"),
    ('k', "deletes to the end of the line in bash and zsh"),
    ('l', "clears the screen in bash and zsh"),
    ('m', "is Enter for programs in the terminal"),
    (
        'n',
        "is the next history entry in bash and zsh, and the next line in emacs",
    ),
    ('o', "opens a line in emacs, and jumps back in vim"),
    (
        'p',
        "is the previous history entry in bash and zsh, and the previous line in emacs",
    ),
    ('q', "resumes output paused with Ctrl+S"),
    ('r', "is reverse search in bash and zsh"),
    ('s', "pauses output, and searches forward in emacs"),
    ('t', "swaps two characters in bash and zsh"),
    ('u', "deletes to the start of the line in bash and zsh"),
    (
        'v',
        "inserts the next key as typed in bash and zsh, and selects a block in vim",
    ),
    (
        'w',
        "deletes the previous word in bash and zsh, and starts window commands in vim",
    ),
    ('x', "starts most commands in emacs"),
    ('y', "pastes the last deleted text in bash and zsh"),
    ('z', "suspends the running program"),
    ('\\', "quits the running program"),
];

/// What programs use well-known Alt (Meta) chords for.
const ALT_USES: [(char, &str); 6] = [
    ('b', "moves back a word in bash, zsh and emacs"),
    ('f', "moves forward a word in bash, zsh and emacs"),
    ('d', "deletes the next word in bash, zsh and emacs"),
    (
        '.',
        "inserts the last argument of the previous command in bash and zsh",
    ),
    ('x', "runs a command by name in emacs"),
    ('w', "copies the selection in emacs"),
];

/// Why binding `chord` would take a key from programs in the terminal, or `None` when they
/// would not normally receive it. On macOS that is any chord without Cmd. Elsewhere: Alt
/// chords (Meta in bash, zsh and emacs), Ctrl chords without Shift on letters, the control
/// symbols, Space and the editing keys, and F keys without Ctrl, Alt or Super. Chords with Super
/// stay with the app; Ctrl+Alt chords are not flagged (AltGr typing is let through).
pub fn risk(mac: bool, chord: Chord) -> Option<String> {
    let label = chord.label(mac);
    let lost = "Programs in the terminal will no longer receive it.";
    let ctrl_only = chord.ctrl && !chord.alt && !chord.shift && !chord.cmd;
    let alt_only = chord.alt && !chord.ctrl && !chord.cmd;
    let known = match chord.key {
        ChordKey::Char(c) if ctrl_only => CTRL_USES.iter().find(|(k, _)| *k == c),
        ChordKey::Char(c) if alt_only && !chord.shift && !mac => {
            ALT_USES.iter().find(|(k, _)| *k == c)
        }
        _ => None,
    };
    if let Some((_, what)) = known {
        return Some(format!("{label} {what}. {lost}"));
    }
    if chord.cmd {
        return None;
    }
    if mac {
        return Some(format!(
            "{label} is a key programs in the terminal can use. {lost}"
        ));
    }
    if alt_only {
        return Some(format!(
            "{label} is a Meta key in bash, zsh and emacs. {lost}"
        ));
    }
    if ctrl_only {
        let control_key = match chord.key {
            ChordKey::Char(c) => c.is_ascii_alphabetic() || "[]\\/@^_2345678".contains(c),
            ChordKey::Space
            | ChordKey::Up
            | ChordKey::Down
            | ChordKey::Left
            | ChordKey::Right
            | ChordKey::Home
            | ChordKey::End
            | ChordKey::PageUp
            | ChordKey::PageDown
            | ChordKey::Backspace
            | ChordKey::Delete
            | ChordKey::Insert
            | ChordKey::Tab
            | ChordKey::Enter => true,
            ChordKey::F(_) | ChordKey::Escape => false,
        };
        if control_key {
            return Some(format!(
                "{label} is a control key programs in the terminal use. {lost}"
            ));
        }
    }
    if !chord.ctrl && !chord.alt && matches!(chord.key, ChordKey::F(_)) {
        return Some(format!(
            "{label}: full-screen programs such as htop and mc use F keys. {lost}"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Chord {
        Chord::parse(s).unwrap().unwrap()
    }

    #[test]
    fn chords_parse_and_format_round_trip() {
        for (text, mac) in [
            ("cmd+t", true),
            ("ctrl+shift+t", false),
            ("ctrl+alt+f", false),
            ("shift+cmd+]", true),
            ("ctrl+alt+shift+super+f5", false),
            ("ctrl+plus", false),
            ("alt+space", false),
            ("ctrl+pageup", false),
            ("f12", true),
            ("cmd+,", true),
        ] {
            let chord = c(text);
            let written = chord.to_config(mac);
            assert_eq!(c(&written), chord, "{text} -> {written}");
        }
        assert_eq!(c("Ctrl + Shift + T").to_config(false), "ctrl+shift+t");
        assert_eq!(c("shift+ctrl+t").to_config(false), "ctrl+shift+t");
        assert_eq!(c("command+option+t").to_config(true), "alt+cmd+t");
        assert_eq!(c("super+t").to_config(true), "cmd+t");
        assert_eq!(c("cmd+t").to_config(false), "super+t");
        assert_eq!(Chord::parse("none"), Ok(None));
        assert_eq!(Chord::parse(" None "), Ok(None));
    }

    #[test]
    fn shifted_symbols_fold_to_their_key_only_with_shift() {
        assert_eq!(c("cmd+shift+}"), c("cmd+shift+]"));
        assert_eq!(c("ctrl+shift+<"), c("ctrl+shift+,"));
        assert_eq!(c("ctrl+shift+{"), c("ctrl+shift+["));
        // Other symbols are keys of their own: on AZERTY `&`, `(` and `!` are unshifted keys,
        // German has `#`; they never stand for a digit.
        for (sym, digit) in [
            ('&', '7'),
            ('!', '1'),
            ('(', '9'),
            ('*', '8'),
            ('$', '4'),
            ('#', '3'),
        ] {
            let pressed = Chord::pressed(ChordKey::Char(sym), true, false, true, false);
            assert_eq!(pressed.key, ChordKey::Char(sym));
            assert_ne!(pressed, c(&format!("ctrl+shift+{digit}")));
        }
        assert_eq!(c("ctrl+shift+!").key, ChordKey::Char('!'));
        // Without Shift a symbol is its own key (on some layouts `<` has a key of its own).
        assert_eq!(c("ctrl+<").key, ChordKey::Char('<'));
        assert_eq!(
            Chord::pressed(ChordKey::Char('}'), false, false, true, true),
            c("cmd+shift+]")
        );
    }

    #[test]
    fn bad_chords_say_why() {
        for (text, why) in [
            ("", "is empty"),
            ("ctrl+", "has no key"),
            ("ctrl++", "empty part"),
            ("hyper+t", "unknown modifier"),
            ("ctrl+enterr", "unknown key"),
            ("ctrl+f25", "unknown key"),
        ] {
            let err = Chord::parse(text).unwrap_err();
            assert!(err.contains(why), "{text}: {err}");
        }
    }

    #[test]
    fn only_chords_with_a_modifier_or_f_keys_may_be_bound() {
        assert!(c("t").allowed().is_err());
        assert!(c("shift+t").allowed().is_err());
        assert!(c("enter").allowed().is_err());
        assert!(c("tab").allowed().is_err());
        assert!(c("ctrl+escape").allowed().is_err());
        assert!(c("escape").allowed().is_err());
        assert!(c("f13").allowed().is_err());
        assert!(c("f1").allowed().is_ok());
        assert!(c("shift+f12").allowed().is_ok());
        assert!(c("ctrl+t").allowed().is_ok());
        assert!(c("alt+t").allowed().is_ok());
        assert!(c("cmd+t").allowed().is_ok());
        assert!(c("ctrl+tab").allowed().is_ok());
    }

    #[test]
    fn labels_per_platform() {
        assert_eq!(c("shift+cmd+,").label(true), "⇧⌘,");
        assert_eq!(c("ctrl+shift+t").label(false), "Ctrl+Shift+T");
        assert_eq!(c("ctrl+alt+f").label(false), "Ctrl+Alt+F");
        assert_eq!(c("ctrl+alt+f").label(true), "⌃⌥F");
        assert_eq!(c("super+t").label(false), "Super+T");
        assert_eq!(c("f5").label(false), "F5");
        assert_eq!(c("cmd+up").label(true), "⌘↑");
    }

    #[test]
    fn every_action_has_a_unique_name() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.name()), Some(a));
        }
        let mut names: Vec<&str> = Action::ALL.iter().map(|a| a.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Action::ALL.len());
        assert_eq!(Action::from_name("nope"), None);
    }

    #[test]
    fn defaults_never_share_a_chord_and_may_all_be_bound() {
        for mac in [true, false] {
            let b = Bindings::defaults(mac);
            let chords: Vec<Chord> = Action::ALL.iter().filter_map(|a| b.chord(*a)).collect();
            for (i, a) in chords.iter().enumerate() {
                assert!(!chords[i + 1..].contains(a), "{} twice", a.label(mac));
                assert!(a.allowed().is_ok(), "{}", a.label(mac));
            }
        }
    }

    #[test]
    fn overrides_replace_or_clear_a_default() {
        let mut o = Overrides::new();
        set_override(true, &mut o, Action::NewTab, Some(c("cmd+shift+t")));
        set_override(true, &mut o, Action::CloseTab, None);
        set_override(true, &mut o, Action::Quit, Some(c("cmd+q")));
        assert!(!o.contains_key(&Action::Quit), "the default is not stored");
        let b = Bindings::new(true, &o);
        assert_eq!(b.chord(Action::NewTab), Some(c("cmd+shift+t")));
        assert_eq!(b.action(c("cmd+shift+t")), Some(Action::NewTab));
        assert_eq!(b.action(c("cmd+t")), None);
        assert_eq!(b.chord(Action::CloseTab), None);
        assert!(!b.is_default(Action::CloseTab));
        assert!(b.is_default(Action::Quit));
        // Unbinding an action that has no default stores nothing.
        set_override(true, &mut o, Action::OpenSettingsFile, None);
        assert!(!o.contains_key(&Action::OpenSettingsFile));
    }

    #[test]
    fn conflicting_overrides_fall_back_to_their_default() {
        let mut k = KeyOverrides::default();
        // Takes Close tab's chord while Close tab keeps it.
        k.map.insert(Action::NewTab, Some(c("cmd+w")));
        // A swap is fine.
        k.map.insert(Action::Find, Some(c("cmd+shift+g")));
        k.map.insert(Action::FindPrevious, Some(c("cmd+f")));
        let dropped = k.resolve_conflicts(true);
        assert_eq!(dropped, [(Action::NewTab, c("cmd+w"), Action::CloseTab)]);
        assert!(!k.map.contains_key(&Action::NewTab));
        assert_eq!(k.map.len(), 2);
        // Two overrides on one chord: both go, one by one.
        let mut k = KeyOverrides::default();
        k.map
            .insert(Action::OpenSettingsFile, Some(c("cmd+shift+o")));
        k.map
            .insert(Action::RevealConfigFolder, Some(c("cmd+shift+o")));
        let dropped = k.resolve_conflicts(true);
        assert_eq!(dropped.len(), 1);
        assert_eq!(k.map.len(), 1);
        // Falling back can clash again: that override goes too.
        let mut k = KeyOverrides::default();
        k.map.insert(Action::NewTab, Some(c("cmd+w")));
        k.map.insert(Action::CloseTab, Some(c("cmd+t")));
        k.map.insert(Action::Quit, Some(c("cmd+t")));
        k.resolve_conflicts(true);
        let b = Bindings::new(true, &k.map);
        let chords: Vec<Chord> = Action::ALL.iter().filter_map(|a| b.chord(*a)).collect();
        for (i, a) in chords.iter().enumerate() {
            assert!(!chords[i + 1..].contains(a), "{} twice", a.label(true));
        }
    }

    #[test]
    fn risky_chords_name_what_is_lost() {
        let r = |mac, s: &str| risk(mac, c(s));
        assert_eq!(
            r(false, "ctrl+r").as_deref(),
            Some(
                "Ctrl+R is reverse search in bash and zsh. Programs in the terminal will no \
                 longer receive it."
            )
        );
        assert!(
            r(true, "ctrl+r")
                .unwrap()
                .starts_with("⌃R is reverse search")
        );
        assert!(r(false, "ctrl+b").unwrap().contains("tmux"));
        assert!(r(false, "ctrl+w").unwrap().contains("vim"));
        assert!(r(false, "alt+f").unwrap().contains("emacs"));
        assert!(r(false, "alt+q").unwrap().contains("Meta"));
        assert!(r(false, "ctrl+[").unwrap().contains("control key"));
        assert!(r(false, "f5").unwrap().contains("F keys"));
        assert!(r(true, "f5").is_some());
        assert!(r(true, "alt+t").is_some());
        assert!(r(true, "ctrl+shift+t").is_some());
        // Not risky: Cmd chords on macOS, Ctrl+Shift, Super and Ctrl+Alt elsewhere.
        assert_eq!(r(true, "cmd+t"), None);
        assert_eq!(r(true, "ctrl+cmd+t"), None);
        assert_eq!(r(false, "ctrl+shift+t"), None);
        assert_eq!(r(false, "super+t"), None);
        assert_eq!(r(false, "ctrl+alt+f"), None);
        assert_eq!(r(false, "ctrl+,"), None);
        assert_eq!(r(false, "ctrl+f5"), None);
    }

    #[test]
    fn no_default_is_risky_except_the_find_bar_keys() {
        for mac in [true, false] {
            for a in Action::ALL {
                if let Some(chord) = default_chord(mac, a) {
                    let risky = risk(mac, chord).is_some();
                    assert_eq!(
                        risky,
                        !mac && a == Action::FindNext,
                        "{} {}",
                        a.name(),
                        chord.label(mac)
                    );
                }
            }
        }
    }

    #[test]
    fn copy_paste_scroll_save_and_the_app_menu_are_reserved() {
        let r = |mac, s: &str| reserved(mac, c(s));
        assert_eq!(r(true, "cmd+c"), Some("Copy"));
        assert_eq!(r(true, "cmd+shift+v"), Some("Paste"));
        assert_eq!(r(false, "ctrl+shift+c"), Some("Copy"));
        assert_eq!(r(false, "ctrl+shift+super+v"), Some("Paste"));
        assert_eq!(r(true, "cmd+home"), Some("Scroll"));
        assert_eq!(r(false, "shift+pageup"), Some("Scroll"));
        assert_eq!(r(true, "cmd+s"), Some("Save in the profile editor"));
        assert_eq!(r(false, "ctrl+s"), Some("Save in the profile editor"));
        assert_eq!(r(true, "cmd+q"), Some("Quit (system)"));
        assert_eq!(r(true, "cmd+h"), Some("Hide (system)"));
        assert_eq!(r(true, "alt+cmd+h"), Some("Hide Others (system)"));
        assert_eq!(r(false, "ctrl+c"), None);
        assert_eq!(r(false, "super+h"), None);
        assert_eq!(r(true, "cmd+shift+h"), None);
        assert_eq!(r(false, "ctrl+home"), None);
        // No default is reserved, except Quit's own Cmd+Q on macOS.
        for mac in [true, false] {
            for a in Action::ALL {
                if let Some(chord) = default_chord(mac, a) {
                    assert_eq!(
                        reserved(mac, chord).is_some(),
                        mac && a == Action::Quit,
                        "{}",
                        a.name()
                    );
                }
            }
        }
    }

    #[test]
    fn fixed_shortcuts_list_the_reserved_chords() {
        for mac in [true, false] {
            let list = fixed_shortcuts(mac);
            let titles: Vec<&str> = list.iter().map(|f| f.title).collect();
            let mut expected = vec![
                "Copy",
                "Paste",
                "Scroll up a page",
                "Scroll down a page",
                "Scroll to top",
                "Scroll to bottom",
                "Save",
            ];
            if mac {
                expected.extend(["Hide fbterm", "Hide others"]);
            }
            assert_eq!(titles, expected);
            // Every chord shown is one `reserved` keeps, under the matching name.
            for f in &list {
                for chord in std::iter::once(f.chord).chain(f.also) {
                    let what = reserved(mac, chord).unwrap_or_else(|| panic!("{}", f.title));
                    let first = f.title.split(' ').next().unwrap();
                    assert!(what.starts_with(first), "{} is {what}", f.title);
                }
            }
        }
        let label = |mac, i: usize| fixed_shortcuts(mac)[i].chord.label(mac);
        assert_eq!(label(false, 0), "Ctrl+Shift+C");
        assert_eq!(label(false, 1), "Ctrl+Shift+V");
        assert_eq!(label(true, 0), "\u{2318}C");
        assert_eq!(label(true, 1), "\u{2318}V");
        assert_eq!(label(false, 2), "Shift+PageUp");
        assert_eq!(fixed_shortcuts(true)[4].also, Some(c("cmd+home")));
        assert_eq!(fixed_shortcuts(false)[4].also, None);
        // Elsewhere the copy row says where Ctrl+C goes and that right-click copies.
        assert!(
            fixed_shortcuts(false)[0]
                .note
                .unwrap()
                .contains("Right-click")
        );
        assert_eq!(fixed_shortcuts(true)[0].note, None);
    }

    #[test]
    fn every_reserved_chord_but_quit_is_a_fixed_shortcut() {
        // Probe every key with every modifier set: each thing `reserved` names (Quit aside,
        // a fixed action row of its own) is named by some chord of the fixed list.
        let keys = ('a'..='z')
            .map(ChordKey::Char)
            .chain(NAMED.iter().map(|(k, _)| *k))
            .chain((1..=12).map(ChordKey::F));
        let keys: Vec<ChordKey> = keys.collect();
        for mac in [true, false] {
            let listed: std::collections::HashSet<&str> = fixed_shortcuts(mac)
                .iter()
                .flat_map(|f| std::iter::once(f.chord).chain(f.also))
                .filter_map(|c| reserved(mac, c))
                .collect();
            for &key in &keys {
                for bits in 0..16u8 {
                    let chord = Chord {
                        key,
                        ctrl: bits & 1 != 0,
                        alt: bits & 2 != 0,
                        shift: bits & 4 != 0,
                        cmd: bits & 8 != 0,
                    };
                    if let Some(what) = reserved(mac, chord)
                        && what != "Quit (system)"
                    {
                        assert!(listed.contains(what), "{what} is not listed (mac: {mac})");
                    }
                }
            }
        }
    }

    #[test]
    fn reserved_chords_and_fixed_actions_are_dropped_from_overrides() {
        let mut k = KeyOverrides::default();
        k.map.insert(Action::NewTab, Some(c("cmd+c")));
        k.map.insert(Action::Quit, Some(c("cmd+shift+q")));
        k.map.insert(Action::Find, None);
        let dropped = k.drop_reserved(true);
        assert_eq!(dropped.len(), 2, "{dropped:?}");
        assert_eq!(k.map.len(), 1);
        assert!(Action::Quit.is_fixed(true));
        assert!(!Action::Quit.is_fixed(false));
        // Off macOS Quit may change.
        let mut k = KeyOverrides::default();
        k.map.insert(Action::Quit, None);
        assert!(k.drop_reserved(false).is_empty());
    }

    #[test]
    fn pane_actions_have_names_and_titles() {
        let expected = [
            (Action::SplitRight, "split_right", "Split right"),
            (Action::SplitDown, "split_down", "Split down"),
            (Action::FocusLeft, "focus_left", "Focus left"),
            (Action::FocusRight, "focus_right", "Focus right"),
            (Action::FocusUp, "focus_up", "Focus up"),
            (Action::FocusDown, "focus_down", "Focus down"),
            (Action::ZoomIn, "zoom_in", "Zoom in"),
            (Action::ZoomOut, "zoom_out", "Zoom out"),
            (Action::ZoomReset, "zoom_reset", "Reset zoom"),
            (Action::MaximizePane, "maximize_pane", "Maximize pane"),
            (Action::FindInTab, "find_in_tab", "Find in this tab"),
            (Action::MergeTab, "merge_tab", "Merge tab into\u{2026}"),
            (Action::MoveTile, "move_tile", "Move tile\u{2026}"),
            (Action::TileToTab, "tile_to_tab", "Move tile to new tab"),
        ];
        for (action, name, title) in expected {
            assert!(Action::ALL.contains(&action), "{name} is listed");
            assert_eq!(action.name(), name);
            assert_eq!(Action::from_name(name), Some(action));
            assert_eq!(action.title(), title);
        }
        assert_eq!(Action::CloseTab.name(), "close_tab");
        assert_eq!(Action::CloseTab.title(), "Close pane");
        assert_eq!(Action::SplitRight.group(), ActionGroup::Panes);
        assert_eq!(Action::MaximizePane.group(), ActionGroup::Panes);
        assert_eq!(ActionGroup::ALL[1], ActionGroup::Panes, "after Tabs");
        assert_eq!(ActionGroup::Panes.title(), "Panes");
    }

    #[test]
    fn edit_schemes_is_listed_without_a_chord() {
        let a = Action::EditSchemes;
        assert!(Action::ALL.contains(&a));
        assert_eq!(a.name(), "edit_schemes");
        assert_eq!(Action::from_name("edit_schemes"), Some(a));
        assert_eq!(a.title(), "Color schemes");
        assert_eq!(a.group(), ActionGroup::Windows);
        assert_eq!(default_chord(true, a), None);
        assert_eq!(default_chord(false, a), None);
    }

    #[test]
    fn file_panel_is_listed_with_its_chord() {
        let a = Action::FilePanel;
        assert!(Action::ALL.contains(&a));
        assert_eq!(a.name(), "file_panel");
        assert_eq!(Action::from_name("file_panel"), Some(a));
        assert_eq!(a.title(), "File panel");
        let d = |mac| default_chord(mac, a).map(|c| c.to_config(mac));
        assert_eq!(d(true).as_deref(), Some("shift+cmd+e"));
        assert_eq!(d(false).as_deref(), Some("ctrl+shift+e"));
        for mac in [true, false] {
            let chord = default_chord(mac, a).unwrap();
            assert_eq!(reserved(mac, chord), None);
        }
    }

    #[test]
    fn tunnels_is_listed_without_a_chord() {
        let a = Action::Tunnels;
        assert!(Action::ALL.contains(&a));
        assert_eq!(a.name(), "tunnels");
        assert_eq!(Action::from_name("tunnels"), Some(a));
        assert_eq!(a.title(), "Tunnels");
        assert_eq!(a.group(), ActionGroup::Windows);
        assert_eq!(default_chord(true, a), None);
        assert_eq!(default_chord(false, a), None);
    }

    #[test]
    fn export_and_import_are_listed_without_a_chord() {
        for (a, name, title) in [
            (
                Action::ExportProfiles,
                "export_profiles",
                "Export profiles\u{2026}",
            ),
            (
                Action::ImportProfiles,
                "import_profiles",
                "Import profiles\u{2026}",
            ),
        ] {
            assert!(Action::ALL.contains(&a));
            assert_eq!(a.name(), name);
            assert_eq!(Action::from_name(name), Some(a));
            assert_eq!(a.title(), title);
            assert_eq!(a.group(), ActionGroup::Windows);
            assert_eq!(default_chord(true, a), None);
            assert_eq!(default_chord(false, a), None);
        }
    }

    #[test]
    fn pane_action_default_chords() {
        let d = |mac, a| default_chord(mac, a).map(|c| c.to_config(mac));
        for (a, mac, other) in [
            (Action::SplitRight, "cmd+d", "ctrl+shift+d"),
            (Action::SplitDown, "shift+cmd+d", "ctrl+alt+shift+d"),
            (Action::FocusLeft, "alt+cmd+left", "ctrl+alt+left"),
            (Action::FocusRight, "alt+cmd+right", "ctrl+alt+right"),
            (Action::FocusUp, "alt+cmd+up", "ctrl+alt+up"),
            (Action::FocusDown, "alt+cmd+down", "ctrl+alt+down"),
            (Action::ZoomIn, "cmd+=", "ctrl+shift+="),
            (Action::ZoomOut, "cmd+-", "ctrl+shift+-"),
            (Action::ZoomReset, "cmd+0", "ctrl+shift+0"),
            (Action::MaximizePane, "shift+cmd+enter", "ctrl+shift+enter"),
        ] {
            assert_eq!(d(true, a).as_deref(), Some(mac), "{}", a.name());
            assert_eq!(d(false, a).as_deref(), Some(other), "{}", a.name());
        }
        for a in [
            Action::FindInTab,
            Action::MergeTab,
            Action::MoveTile,
            Action::TileToTab,
        ] {
            assert_eq!(default_chord(true, a), None);
            assert_eq!(default_chord(false, a), None);
        }
        // The plus key zooms in however it is typed: `plus` and `=` are one chord, with or
        // without Shift, except that a bare `Ctrl+=` is not one (it stays with programs).
        let plus = |a: &str, b: &str| c(a).same_plus_key(c(b));
        assert!(plus("ctrl+shift+plus", "ctrl+shift+="));
        assert!(plus("ctrl+plus", "ctrl+shift+="));
        assert!(
            !plus("ctrl+=", "ctrl+shift+="),
            "bare Ctrl+= stays with programs"
        );
        assert!(plus("cmd+=", "cmd+shift+="));
        assert!(plus("cmd+plus", "cmd+="));
        assert!(!plus("cmd+=", "alt+cmd+="));
        assert!(!plus("cmd+-", "cmd+="));
    }
}
