//! The Color schemes window's state: every scheme as a draft, the selected one's form (colors,
//! font, name) and a live preview terminal, as plain data so its rules are testable without a
//! GUI. Built-in schemes are read-only; user schemes are edited in their draft and written to
//! `schemes/` only on Save, so unsaved edits change the preview and nothing else. The window is
//! drawn by [`view`] and wired to the app in `glue`.

use std::path::PathBuf;
use std::sync::Arc;

use fbterm_config::Rgb;
use fbterm_config::schemes::{Scheme, SchemeColors, SchemeSet, contrast_ok, new_user_id};
use fbterm_term::{ColorScheme, GridSize, Selection, SharedTerminal, Terminal, TerminalStyle};
use parking_lot::Mutex;

use crate::settings_window::FontChoice;

pub use gallery::{GalleryLoad, UpdateChoice};

pub mod gallery;
#[cfg(test)]
mod gallery_tests;
mod glue;
#[cfg(test)]
mod tests;
pub mod view;

/// The hint under the preview when a user scheme misses the readability bar of the built-ins.
pub const CONTRAST_HINT: &str = "Text may be hard to read on this background.";
/// The first entry of the font family list: the scheme sets no font.
pub const USE_GLOBAL_FONT: &str = "Use global font";
/// The first entry of the font size list: the scheme sets no size.
pub const USE_GLOBAL_SIZE: &str = "Use global size";
/// Sizes offered in the font size list (a scheme file may hold any other).
pub const SIZES: [f32; 12] = [
    9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0,
];

/// One color of a scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entry {
    Foreground,
    Background,
    Cursor,
    Selection,
    /// ANSI color `0..=15`.
    Ansi(u8),
}

const ANSI_NAMES: [&str; 16] = [
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "White",
    "Bright black",
    "Bright red",
    "Bright green",
    "Bright yellow",
    "Bright blue",
    "Bright magenta",
    "Bright cyan",
    "Bright white",
];

impl Entry {
    /// Every entry, in the order of the form.
    pub const ALL: [Entry; 20] = [
        Entry::Foreground,
        Entry::Background,
        Entry::Cursor,
        Entry::Selection,
        Entry::Ansi(0),
        Entry::Ansi(1),
        Entry::Ansi(2),
        Entry::Ansi(3),
        Entry::Ansi(4),
        Entry::Ansi(5),
        Entry::Ansi(6),
        Entry::Ansi(7),
        Entry::Ansi(8),
        Entry::Ansi(9),
        Entry::Ansi(10),
        Entry::Ansi(11),
        Entry::Ansi(12),
        Entry::Ansi(13),
        Entry::Ansi(14),
        Entry::Ansi(15),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Foreground => "Foreground",
            Self::Background => "Background",
            Self::Cursor => "Cursor",
            Self::Selection => "Selection",
            Self::Ansi(n) => ANSI_NAMES[usize::from(n.min(15))],
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Foreground => 0,
            Self::Background => 1,
            Self::Cursor => 2,
            Self::Selection => 3,
            Self::Ansi(n) => 4 + usize::from(n.min(15)),
        }
    }

    pub fn get(self, c: &SchemeColors) -> Rgb {
        match self {
            Self::Foreground => c.foreground,
            Self::Background => c.background,
            Self::Cursor => c.cursor,
            Self::Selection => c.selection,
            Self::Ansi(n) => c.ansi[usize::from(n.min(15))],
        }
    }

    pub fn set(self, c: &mut SchemeColors, v: Rgb) {
        match self {
            Self::Foreground => c.foreground = v,
            Self::Background => c.background = v,
            Self::Cursor => c.cursor = v,
            Self::Selection => c.selection = v,
            Self::Ansi(n) => c.ansi[usize::from(n.min(15))] = v,
        }
    }
}

/// The answer to an import whose id is already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportChoice {
    Replace,
    KeepBoth,
    Cancel,
}

/// What the Color schemes window asks for. `Open`, `Close`, `Duplicate`, `Delete`,
/// `ConfirmDelete`, `Save`, the import and export messages and `Preview` are the app's; the
/// rest edit the window's state.
#[derive(Debug, Clone)]
pub enum SchemesMsg {
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "windows open through open_schemes; the message is for tests and callers that only have a message"
        )
    )]
    Open,
    Close,
    Select(String),
    /// Copy the selected scheme as a new user scheme (saved at once) and select it.
    Duplicate,
    /// Show the name as a field.
    RenameStart,
    /// The typed name.
    Rename(String),
    /// Done typing the name.
    RenameEnd,
    Delete,
    ConfirmDelete,
    CancelDelete,
    /// Show the sliders for this entry.
    Edit(Entry),
    Color(Entry, Rgb),
    /// The typed hex value of an entry; applied once it reads as `#rrggbb`.
    Hex(Entry, String),
    FontFamily(Option<FontChoice>),
    FontSize(Option<f32>),
    Save,
    Import,
    Imported(Option<PathBuf>),
    /// The chosen file as read on the blocking pool (the error text names file and line).
    ImportRead(Box<Result<Scheme, String>>),
    ImportChoice(ImportChoice),
    Export,
    Exported(Option<PathBuf>),
    /// Something the preview terminal reported (only a resize matters).
    Preview(fbterm_term::TerminalOutput),
    /// Show (true) or leave the gallery; showing it fetches the catalog when none is loaded.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "sent by the gallery view, which comes next")
    )]
    ShowGallery(bool),
    /// Fetch the catalog again (also the Retry after a failure).
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "sent by the gallery view, which comes next")
    )]
    RefreshGallery,
    /// The catalog as fetched on the blocking pool.
    GalleryFetched(Result<Vec<u8>, String>),
    /// Install the catalog theme with this id.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "sent by the gallery view, which comes next")
    )]
    Install(String),
    /// Update the installed copy of the catalog theme with this id.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "sent by the gallery view, which comes next")
    )]
    Update(String),
    /// The answer to the question about updating an edited theme.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "sent by the gallery view, which comes next")
    )]
    UpdateChoice(UpdateChoice),
    /// A credit's project link, for the system browser (`https://` only).
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "sent by the gallery view, which comes next")
    )]
    OpenCreditLink(String),
}

/// The delete question for a scheme, with how many profiles use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAsk {
    pub id: String,
    pub name: String,
    pub profiles: usize,
}

impl DeleteAsk {
    pub fn title(&self) -> String {
        format!("Delete \"{}\"?", self.name)
    }

    /// What happens to the profiles that use it; nothing to say when none do.
    pub fn body(&self) -> Option<String> {
        match self.profiles {
            0 => None,
            1 => Some("1 profile uses it and will switch to the global scheme.".into()),
            n => Some(format!(
                "{n} profiles use it and will switch to the global scheme."
            )),
        }
    }
}

/// The import question: a file whose id is already a user scheme.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportAsk {
    /// The scheme as read from the file.
    pub scheme: Scheme,
    /// The name of the scheme it would replace.
    pub existing: String,
}

impl ImportAsk {
    pub fn title(&self) -> String {
        format!("\"{}\" is already installed", self.scheme.name)
    }

    pub fn body(&self) -> String {
        format!(
            "Replace \"{}\" with the imported scheme, or keep both?",
            self.existing
        )
    }
}

/// One entry of the font family list: a family, or `Use global font` (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontPick(pub Option<FontChoice>);

impl std::fmt::Display for FontPick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(c) => c.fmt(f),
            None => f.write_str(USE_GLOBAL_FONT),
        }
    }
}

/// One entry of the font size list: a size, or `Use global size` (`None`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizePick(pub Option<f32>);

impl std::fmt::Display for SizePick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(s) => write!(f, "{} pt", crate::settings_window::size_text(s)),
            None => f.write_str(USE_GLOBAL_SIZE),
        }
    }
}

/// What the preview shows: a prompt, `ls`-style colored names, the eight colors and their
/// bright versions as words, a warning and an error, and a prompt waiting at the cursor.
const SAMPLE: &[u8] = b"\x1b[32muser@sample\x1b[0m:\x1b[34m~/project\x1b[0m$ ls\r\n\
\x1b[1;34mbuild\x1b[0m  \x1b[1;34mdocs\x1b[0m  \x1b[32mrun.sh\x1b[0m  \x1b[36mlatest\x1b[0m  README.md  notes.txt\r\n\
\x1b[30mblack\x1b[0m \x1b[31mred\x1b[0m \x1b[32mgreen\x1b[0m \x1b[33myellow\x1b[0m \x1b[34mblue\x1b[0m \x1b[35mmagenta\x1b[0m \x1b[36mcyan\x1b[0m \x1b[37mwhite\x1b[0m\r\n\
\x1b[90mblack\x1b[0m \x1b[91mred\x1b[0m \x1b[92mgreen\x1b[0m \x1b[93myellow\x1b[0m \x1b[94mblue\x1b[0m \x1b[95mmagenta\x1b[0m \x1b[96mcyan\x1b[0m \x1b[97mwhite\x1b[0m\r\n\
\x1b[33mwarning:\x1b[0m disk 81% full  \x1b[31merror:\x1b[0m connection refused\r\n\
\x1b[32muser@sample\x1b[0m:\x1b[34m~/project\x1b[0m$ ";

/// The selected word of the sample: `README.md`, line 1, columns 29 to 37.
const SAMPLE_SELECTION: (i32, usize, usize) = (1, 29, 37);

/// A fresh preview terminal of `size` showing the sample, its word selected.
fn preview_terminal(size: GridSize) -> SharedTerminal {
    let mut term = Terminal::new(size, 100);
    term.feed(SAMPLE);
    let history = i32::try_from(term.history_len()).unwrap_or(i32::MAX);
    let (line, first, last) = SAMPLE_SELECTION;
    term.set_selection(Some(Selection::on_line(line - history, first, last)));
    Arc::new(Mutex::new(term))
}

pub struct SchemesState {
    /// Every scheme as edited here: the set's order, then unsaved new ones.
    drafts: Vec<Scheme>,
    /// Every scheme as last loaded, to tell which drafts have unsaved edits.
    saved: Vec<Scheme>,
    /// The id of the selected scheme.
    pub selected: String,
    /// The entry whose sliders show.
    pub editing: Entry,
    /// The hex field of each entry of the selected scheme, as typed.
    hex: Vec<String>,
    /// The name shows as a field.
    pub renaming: bool,
    /// The delete question, while it is asked.
    pub confirm: Option<DeleteAsk>,
    /// The import question, while it is asked.
    pub import: Option<ImportAsk>,
    /// The outcome of the last action (`Saved`), shown in the footer.
    pub notice: Option<String>,
    /// What went wrong with the last action, shown in the footer.
    pub error: Option<String>,
    /// Installed monospace families, for the font list.
    installed: Vec<String>,
    /// The preview terminal (no session: fed the sample once per size).
    pub preview: SharedTerminal,
    /// The theme gallery's catalog, or why there is none.
    pub gallery: GalleryLoad,
    /// The gallery shows instead of the scheme editor.
    pub showing_gallery: bool,
    /// The catalog id of an edited theme whose update is being asked about.
    pub pending_update: Option<String>,
}

impl SchemesState {
    /// Every scheme of `set`, `select` selected if known (else the first).
    pub fn new(set: &SchemeSet, select: Option<&str>, installed: Vec<String>) -> Self {
        let mut state = Self {
            drafts: set.all().to_vec(),
            saved: set.all().to_vec(),
            selected: String::new(),
            editing: Entry::Background,
            hex: Vec::new(),
            renaming: false,
            confirm: None,
            import: None,
            notice: None,
            error: None,
            installed,
            preview: preview_terminal(crate::app::DEFAULT_GRID),
            gallery: GalleryLoad::Idle,
            showing_gallery: false,
            pending_update: None,
        };
        let first = state
            .drafts
            .first()
            .map(|s| s.id.clone())
            .unwrap_or_default();
        state.select(
            select
                .filter(|id| state.draft(id).is_some())
                .map_or(first, str::to_owned),
        );
        state
    }

    pub fn drafts(&self) -> &[Scheme] {
        &self.drafts
    }

    pub fn draft(&self, id: &str) -> Option<&Scheme> {
        self.drafts.iter().find(|s| s.id == id)
    }

    pub fn selected_draft(&self) -> Option<&Scheme> {
        self.draft(&self.selected)
    }

    fn selected_mut(&mut self) -> Option<&mut Scheme> {
        let id = &self.selected;
        self.drafts.iter_mut().find(|s| s.id == *id)
    }

    /// Whether the selected scheme can be changed (a user scheme).
    pub fn editable(&self) -> bool {
        self.selected_draft().is_some_and(|s| !s.builtin)
    }

    /// Whether the scheme `id` has edits not saved yet (or was never saved).
    pub fn is_dirty(&self, id: &str) -> bool {
        let saved = self.saved.iter().find(|s| s.id == id);
        saved != self.draft(id)
    }

    /// The hex field of `entry`, as typed.
    pub fn hex_text(&self, entry: Entry) -> &str {
        self.hex.get(entry.index()).map_or("", String::as_str)
    }

    /// Whether the hex field of `entry` holds a color (else it is drawn as an error).
    pub fn hex_valid(&self, entry: Entry) -> bool {
        self.hex_text(entry).trim().parse::<Rgb>().is_ok()
    }

    /// The contrast hint for the selected scheme, if it is a user scheme under the bar.
    pub fn contrast_hint(&self) -> Option<&'static str> {
        self.selected_draft()
            .filter(|s| !s.builtin && !contrast_ok(&s.colors))
            .map(|_| CONTRAST_HINT)
    }

    /// The font family list: `Use global font`, then the families (a family the scheme names
    /// that is not installed is listed, marked).
    pub fn font_options(&self) -> Vec<FontPick> {
        let current = self
            .selected_draft()
            .and_then(|s| s.font_family.clone())
            .unwrap_or_else(|| crate::settings_window::BUNDLED_FONT.to_owned());
        std::iter::once(FontPick(None))
            .chain(
                crate::settings_window::font_choices(self.installed.clone(), &current)
                    .into_iter()
                    .map(|c| FontPick(Some(c))),
            )
            .collect()
    }

    pub fn selected_font(&self) -> FontPick {
        let family = self.selected_draft().and_then(|s| s.font_family.clone());
        match family {
            None => FontPick(None),
            Some(f) => self
                .font_options()
                .into_iter()
                .find(|p| p.0.as_ref().is_some_and(|c| c.family == f))
                .unwrap_or(FontPick(Some(FontChoice {
                    family: f,
                    installed: false,
                }))),
        }
    }

    /// The font size list: `Use global size`, then [`SIZES`] (and the scheme's own size when it
    /// is not one of them).
    pub fn size_options(&self) -> Vec<SizePick> {
        let mut sizes: Vec<f32> = SIZES.to_vec();
        if let Some(s) = self.selected_draft().and_then(|s| s.font_size)
            && !sizes.contains(&s)
        {
            sizes.push(s);
            sizes.sort_by(f32::total_cmp);
        }
        std::iter::once(SizePick(None))
            .chain(sizes.into_iter().map(|s| SizePick(Some(s))))
            .collect()
    }

    pub fn selected_size(&self) -> SizePick {
        SizePick(self.selected_draft().and_then(|s| s.font_size))
    }

    /// How the preview draws the selected draft: its colors, its font and size where it sets
    /// them (else `base`'s, the global ones), with its own background.
    pub fn preview_style(&self, base: &TerminalStyle) -> TerminalStyle {
        let (lo, hi) = fbterm_config::settings::FONT_SIZE_RANGE;
        let Some(d) = self.selected_draft() else {
            return base.clone();
        };
        TerminalStyle {
            scheme: ColorScheme::from(&d.colors),
            font: d.font_family.as_deref().map_or(base.font, |f| {
                iced::Font::with_name(crate::settings_window::intern(f))
            }),
            font_size: d.font_size.map_or(base.font_size, |s| s.clamp(lo, hi)),
            accent: None,
            focused: false,
            background_opacity: 1.0,
        }
    }

    /// Select `id`: its hex fields show its colors.
    pub(super) fn select(&mut self, id: String) {
        self.selected = id;
        self.renaming = false;
        self.sync_hex();
    }

    fn sync_hex(&mut self) {
        let colors = self.selected_draft().map(|s| s.colors);
        self.hex = Entry::ALL
            .iter()
            .map(|e| colors.map(|c| e.get(&c).to_string()).unwrap_or_default())
            .collect();
    }

    /// Apply a message that only edits the window's state; the app's messages are ignored here.
    /// Edits of a built-in scheme are refused.
    pub fn update(&mut self, msg: SchemesMsg) {
        if !matches!(msg, SchemesMsg::Preview(_)) {
            self.notice = None;
        }
        match msg {
            SchemesMsg::Select(id) => {
                if self.draft(&id).is_some() {
                    self.error = None;
                    self.select(id);
                }
            }
            SchemesMsg::Edit(entry) => self.editing = entry,
            SchemesMsg::CancelDelete => self.confirm = None,
            SchemesMsg::ImportChoice(ImportChoice::Cancel) => self.import = None,
            SchemesMsg::RenameEnd => self.renaming = false,
            msg if !self.editable() => drop(msg),
            SchemesMsg::RenameStart => self.renaming = true,
            SchemesMsg::Rename(name) => {
                if let Some(d) = self.selected_mut() {
                    d.name = name;
                }
            }
            SchemesMsg::Color(entry, rgb) => {
                self.editing = entry;
                if let Some(d) = self.selected_mut() {
                    entry.set(&mut d.colors, rgb);
                }
                if let Some(h) = self.hex.get_mut(entry.index()) {
                    *h = rgb.to_string();
                }
            }
            SchemesMsg::Hex(entry, text) => {
                self.editing = entry;
                let parsed = text.trim().parse::<Rgb>();
                if let Some(h) = self.hex.get_mut(entry.index()) {
                    *h = text;
                }
                if let (Ok(rgb), Some(d)) = (parsed, self.selected_mut()) {
                    entry.set(&mut d.colors, rgb);
                }
            }
            SchemesMsg::FontFamily(choice) => {
                if let Some(d) = self.selected_mut() {
                    d.font_family = choice.map(|c| c.family);
                }
            }
            SchemesMsg::FontSize(size) => {
                if let Some(d) = self.selected_mut() {
                    d.font_size = size.filter(|s| s.is_finite() && *s > 0.0);
                }
            }
            _ => {}
        }
    }

    /// Copy the selected draft as a new user scheme, `"{name} copy"` with a fresh id, and
    /// select it. Returns the copy for the app to save.
    pub fn duplicate(&mut self) -> Option<Scheme> {
        let source = self.selected_draft()?;
        let copy = Scheme {
            id: new_user_id(),
            name: format!("{} copy", source.name.trim()),
            builtin: false,
            gallery: None,
            ..source.clone()
        };
        self.drafts.push(copy.clone());
        self.select(copy.id.clone());
        Some(copy)
    }

    /// `scheme` was written: its draft has no unsaved edits now.
    pub fn mark_saved(&mut self, scheme: &Scheme) {
        if let Some(d) = self.drafts.iter_mut().find(|d| d.id == scheme.id) {
            *d = scheme.clone();
        }
        match self.saved.iter_mut().find(|s| s.id == scheme.id) {
            Some(s) => *s = scheme.clone(),
            None => self.saved.push(scheme.clone()),
        }
    }

    /// The schemes were loaded again: drafts without edits take the new version, new schemes
    /// appear, removed ones go (unless they hold edits), and a selection that went selects the
    /// first scheme.
    pub fn refresh(&mut self, set: &SchemeSet) {
        let mut drafts: Vec<Scheme> = set
            .all()
            .iter()
            .map(|s| match self.draft(&s.id) {
                Some(d) if self.is_dirty(&s.id) => d.clone(),
                _ => s.clone(),
            })
            .collect();
        let unsaved: Vec<Scheme> = self
            .drafts
            .iter()
            .filter(|d| set.get(&d.id).is_none() && self.is_dirty(&d.id))
            .cloned()
            .collect();
        drafts.extend(unsaved);
        let before = self.selected_draft().cloned();
        self.drafts = drafts;
        self.saved = set.all().to_vec();
        if self.selected_draft().is_none() {
            let first = self
                .drafts
                .first()
                .map(|s| s.id.clone())
                .unwrap_or_default();
            self.select(first);
        } else if self.selected_draft() != before.as_ref() {
            self.sync_hex();
        }
    }

    /// The scheme `id` was deleted: drop it and select `next`.
    pub fn remove(&mut self, id: &str, next: &str) {
        self.drafts.retain(|d| d.id != id);
        self.saved.retain(|d| d.id != id);
        let next = if self.draft(next).is_some() {
            next.to_owned()
        } else {
            self.drafts
                .first()
                .map(|s| s.id.clone())
                .unwrap_or_default()
        };
        self.select(next);
    }

    /// The preview was laid out at `size`: show the sample again at that size (the widget
    /// rewrapped it).
    pub fn resize_preview(&mut self, size: GridSize) {
        self.preview = preview_terminal(size);
    }
}
