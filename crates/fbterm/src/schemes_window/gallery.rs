//! The theme gallery of the Color schemes window: fetching the catalog (only when the user
//! opens the gallery or asks for a refresh), and installing and updating its themes as
//! `gallery-<id>` scheme files. The catalog is untrusted input, parsed by
//! `fbterm_config::catalog`; the last good one is kept on the app for the rest of the run,
//! never on disk.

use std::sync::Arc;

use fbterm_config::catalog::{
    CATALOG_URL, Catalog, CatalogError, CatalogTheme, MAX_CATALOG_BYTES, Status, parse_catalog,
    status,
};
use fbterm_config::schemes::{Credit, Scheme, SchemeColors, SchemeSet, new_user_id, save_user};
use iced::Task;

use super::SchemesMsg;
use crate::app::{App, Message};

/// The start of the line shown when the catalog could not be loaded.
pub const LOAD_FAILED: &str = "Couldn't load the theme gallery";

/// Where the gallery stands in the Color schemes window.
#[derive(Debug, Clone, Default)]
pub enum GalleryLoad {
    /// Never opened in this run.
    #[default]
    Idle,
    /// The catalog is being fetched.
    Loading,
    Loaded(Catalog),
    /// The one line to show instead of the catalog.
    Failed(String),
}

/// The answer to an Update (or Install) over a scheme the user edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChoice {
    /// Replace my changes.
    Replace,
    /// Install as a copy: a new user scheme with the new colors and the credit.
    Copy,
    Cancel,
}

/// The line of the question about updating a theme edited here.
pub const EDITED_HERE: &str = "You changed this theme on this machine.";

/// The note on a withdrawn theme that is still installed.
pub const NO_LONGER_OFFERED: &str = "No longer offered";

/// The action a gallery row offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowButton {
    Install,
    /// Installed at the catalog's version: shown, not pressable.
    Installed,
    Update,
}

/// One theme as the gallery lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct GalleryRow {
    /// The catalog id.
    pub id: String,
    pub name: String,
    /// `"<project> by <author>, <license>"`.
    pub credit_line: String,
    /// The credit's project page, when it may be opened.
    pub link: Option<String>,
    /// `variant = "dark"`.
    pub dark: bool,
    pub button: Option<RowButton>,
    pub note: Option<&'static str>,
    pub colors: SchemeColors,
}

/// `s` in at most `max` characters: cut with an ellipsis (spaces before it dropped) when
/// longer, so a long catalog name stays on one line.
pub fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", kept.trim_end())
}

/// A credit as one line: `"<project> by <author>, <license>"`.
pub fn credit_line(credit: &Credit) -> String {
    format!(
        "{} by {}, {}",
        credit.project.trim(),
        credit.author.trim(),
        credit.license.trim()
    )
}

/// The rows of `catalog` against the `installed` schemes, sorted by name (ignoring case).
/// `filter` keeps the themes whose name, credit project or credit author contains it
/// (ignoring case and surrounding spaces); `variant` keeps only dark (`Some(true)`) or light
/// (`Some(false)`) themes. A withdrawn theme shows only while installed, with a note.
pub fn gallery_rows(
    catalog: &Catalog,
    installed: &SchemeSet,
    filter: &str,
    variant: Option<bool>,
) -> Vec<GalleryRow> {
    let needle = filter.trim().to_lowercase();
    let matches = |t: &CatalogTheme| {
        needle.is_empty()
            || std::iter::once(t.scheme.name.as_str())
                .chain(
                    t.scheme
                        .credit
                        .iter()
                        .flat_map(|c| [c.project.as_str(), c.author.as_str()]),
                )
                .any(|field| field.to_lowercase().contains(&needle))
    };
    let mut rows: Vec<GalleryRow> = catalog
        .themes
        .iter()
        .filter(|t| variant.is_none_or(|dark| t.dark == dark) && matches(t))
        .filter_map(|t| {
            let (button, note) = match status(t, installed.get(&t.scheme.id)) {
                Status::Withdrawn { installed: false } => return None,
                Status::Withdrawn { installed: true } => (None, Some(NO_LONGER_OFFERED)),
                Status::Available => (Some(RowButton::Install), None),
                Status::Installed => (Some(RowButton::Installed), None),
                Status::Update { .. } => (Some(RowButton::Update), None),
            };
            let credit = t.scheme.credit.as_ref();
            Some(GalleryRow {
                id: t.id.clone(),
                name: t.scheme.name.trim().to_owned(),
                credit_line: credit.map(credit_line).unwrap_or_default(),
                link: credit.map(|c| c.url.clone()).filter(|u| credit_link_ok(u)),
                dark: t.dark,
                button,
                note,
                colors: t.scheme.colors,
            })
        })
        .collect();
    rows.sort_by_cached_key(|r| (r.name.to_lowercase(), r.id.clone()));
    rows
}

/// A blocking catalog fetch: `(url, size cap)` to the body.
pub type FetchFn = dyn Fn(&str, usize) -> Result<Vec<u8>, String> + Send + Sync;

/// The gallery's ways out of the app, replaceable so tests never touch the network or start
/// a browser.
#[derive(Clone)]
pub struct GalleryIo {
    /// Fetch the catalog; blocking, so it runs on the blocking pool.
    pub fetch: Arc<FetchFn>,
    /// Open a credit link in the system browser (only `https://` links reach it).
    pub open: Arc<dyn Fn(&str) + Send + Sync>,
}

impl Default for GalleryIo {
    /// The network and the system opener; in unit tests, a fetch that always fails and an
    /// opener that does nothing, so a test that forgets its fakes still stays offline.
    fn default() -> Self {
        if cfg!(test) {
            return Self {
                fetch: Arc::new(|_, _| Err("no network in tests".into())),
                open: Arc::new(|_| {}),
            };
        }
        Self {
            fetch: Arc::new(net_fetch),
            open: Arc::new(|url| {
                crate::sessions::spawn_reaped(crate::sessions::opener(url), url);
            }),
        }
    }
}

/// The catalog over HTTPS through the shared [`crate::net::get`], with fbterm's User-Agent.
fn net_fetch(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let agent = format!("fbterm/{}", env!("CARGO_PKG_VERSION"));
    crate::net::get(
        url,
        &[("User-Agent", agent.as_str())],
        u64::try_from(max_bytes).unwrap_or(u64::MAX),
    )
}

/// Whether a credit link may be opened: `https://` with a host, and no whitespace or control
/// characters (the opener gets it as one argument, never through a shell).
pub fn credit_link_ok(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && !rest.chars().any(|c| c.is_whitespace() || c.is_control())
    })
}

/// The gallery's line for a fetch that failed or a catalog that could not be used.
fn failure_line(reason: &str) -> String {
    format!("{LOAD_FAILED}: {reason}")
}

/// The window's state for a fetched catalog: loaded, or the line to show.
fn load_result(result: Result<Vec<u8>, String>) -> GalleryLoad {
    let bytes = match result {
        Ok(b) => b,
        Err(e) => {
            tracing::info!(error = %e, "theme gallery not loaded");
            return GalleryLoad::Failed(failure_line(&e));
        }
    };
    match parse_catalog(&bytes) {
        Ok(catalog) => {
            for skipped in &catalog.skipped {
                tracing::info!(theme = %skipped, "gallery theme skipped");
            }
            tracing::info!(themes = catalog.themes.len(), "theme gallery loaded");
            GalleryLoad::Loaded(catalog)
        }
        Err(e) => {
            tracing::info!(error = %e, "theme catalog refused");
            GalleryLoad::Failed(match e {
                CatalogError::NewerFormat(_) => e.to_string(),
                _ => failure_line(&e.to_string()),
            })
        }
    }
}

impl App {
    /// The fetch as the blocking pool runs it.
    pub(crate) fn gallery_job(
        &self,
    ) -> impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static + use<> {
        let fetch = self.gallery_io.fetch.clone();
        move || fetch(CATALOG_URL, MAX_CATALOG_BYTES)
    }

    /// Fetch the catalog on the blocking pool, unless a fetch is under way (in this or an
    /// earlier Color schemes window); the result comes back as
    /// [`SchemesMsg::GalleryFetched`] with this fetch's generation. A question about an
    /// edited theme is dropped: its catalog is about to change.
    fn fetch_gallery(&mut self) -> Task<Message> {
        if self.gallery_loading {
            return Task::none();
        }
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        state.gallery = GalleryLoad::Loading;
        state.pending_update = None;
        self.gallery_loading = true;
        self.gallery_generation += 1;
        let generation = self.gallery_generation;
        let job = self.gallery_job();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(job)
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |r| Message::Schemes(SchemesMsg::GalleryFetched(generation, r)),
        )
    }

    /// The gallery's messages; any other is handed back.
    pub(super) fn gallery_msg(&mut self, msg: SchemesMsg) -> Result<Task<Message>, SchemesMsg> {
        let task = match msg {
            SchemesMsg::ShowGallery(show) => self.show_gallery(show),
            SchemesMsg::RefreshGallery => self.fetch_gallery(),
            SchemesMsg::GalleryFetched(generation, result) => {
                self.gallery_fetched(generation, result);
                Task::none()
            }
            SchemesMsg::Install(id) | SchemesMsg::Update(id) => self.install_theme(&id),
            SchemesMsg::UpdateChoice(choice) => self.update_choice(choice),
            SchemesMsg::OpenCreditLink(url) => {
                self.open_credit_link(&url);
                Task::none()
            }
            other => return Err(other),
        };
        Ok(task)
    }

    /// Show or hide the gallery. Showing it the first time in a run (or after a failure)
    /// fetches the catalog; nothing else does, but Refresh.
    fn show_gallery(&mut self, show: bool) -> Task<Message> {
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        state.showing_gallery = show;
        if show && matches!(state.gallery, GalleryLoad::Idle | GalleryLoad::Failed(_)) {
            return self.fetch_gallery();
        }
        Task::none()
    }

    /// A fetch finished. Only the latest fetch counts; an older result is dropped. A good
    /// catalog is kept for the rest of the run; a failure shows its line and leaves installed
    /// schemes alone.
    fn gallery_fetched(&mut self, generation: u64, result: Result<Vec<u8>, String>) {
        if generation != self.gallery_generation || !self.gallery_loading {
            tracing::debug!(generation, "stale theme catalog result dropped");
            return;
        }
        self.gallery_loading = false;
        let load = load_result(result);
        if let GalleryLoad::Loaded(catalog) = &load {
            self.gallery_catalog = Some(catalog.clone());
        }
        if let Some((_, state)) = &mut self.schemes_win {
            state.gallery = load;
        }
    }

    /// The loaded catalog's theme `id`, if the gallery shows a catalog.
    fn catalog_theme(&self, id: &str) -> Option<CatalogTheme> {
        let (_, state) = self.schemes_win.as_ref()?;
        match &state.gallery {
            GalleryLoad::Loaded(c) => c.themes.iter().find(|t| t.id == id).cloned(),
            _ => None,
        }
    }

    /// Install or Update theme `id`: written in place when nothing would be lost, asked
    /// about when the installed scheme was edited (or its file has no gallery block or does
    /// not load, or the window holds unsaved edits of it), and nothing at all when the theme
    /// is installed already or withdrawn.
    fn install_theme(&mut self, id: &str) -> Task<Message> {
        let Some(theme) = self.catalog_theme(id) else {
            return Task::none();
        };
        let installed = self.schemes.get(&theme.scheme.id);
        let mut st = status(&theme, installed);
        if st == Status::Available && self.scheme_file_exists(&theme.scheme.id) {
            // A file that did not load would be overwritten: treat it as edited.
            st = Status::Update { edited: true };
        }
        let unsaved = self
            .schemes_win
            .as_ref()
            .is_some_and(|(_, state)| state.is_dirty(&theme.scheme.id));
        if unsaved && matches!(st, Status::Available | Status::Update { .. }) {
            st = Status::Update { edited: true };
        }
        let updating = installed.is_some();
        match st {
            Status::Installed | Status::Withdrawn { .. } => Task::none(),
            Status::Update { edited: true } => {
                if let Some((_, state)) = &mut self.schemes_win {
                    state.notice = None;
                    state.error = None;
                    state.pending_update = Some(theme.id);
                }
                Task::none()
            }
            Status::Available | Status::Update { edited: false } => {
                self.write_theme(theme.scheme, updating)
            }
        }
    }

    fn scheme_file_exists(&self, scheme_id: &str) -> bool {
        self.paths
            .schemes()
            .join(format!("{scheme_id}.toml"))
            .exists()
    }

    /// The answer to the edited-update question.
    fn update_choice(&mut self, choice: UpdateChoice) -> Task<Message> {
        let Some(id) = self
            .schemes_win
            .as_mut()
            .and_then(|(_, state)| state.pending_update.take())
        else {
            return Task::none();
        };
        let Some(theme) = self.catalog_theme(&id) else {
            return Task::none();
        };
        match choice {
            UpdateChoice::Cancel => Task::none(),
            UpdateChoice::Replace => self.write_theme(theme.scheme, true),
            UpdateChoice::Copy => {
                let copy = Scheme {
                    id: new_user_id(),
                    name: format!("{} copy", theme.scheme.name.trim()),
                    gallery: None,
                    ..theme.scheme
                };
                self.write_theme(copy, false)
            }
        }
    }

    /// Write `scheme` to `schemes/` and load the set again, which recolors the open panes
    /// whose colors changed.
    fn write_theme(&mut self, scheme: Scheme, updating: bool) -> Task<Message> {
        let dir = self.paths.schemes();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        state.notice = None;
        if let Err(e) = save_user(&dir, &scheme) {
            tracing::warn!(error = %e, id = %scheme.id, "gallery theme not saved");
            state.error = Some(format!("Could not install: {e}"));
            return Task::none();
        }
        tracing::info!(id = %scheme.id, updating, "gallery theme installed");
        state.error = None;
        let verb = if updating { "Updated" } else { "Installed" };
        state.notice = Some(format!("{verb} \"{}\"", scheme.name.trim()));
        // The file is the truth now: an open draft of the same id (unsaved edits too, which
        // Replace was asked about) is discarded for this version.
        state.discard_draft(&scheme);
        // Reloading compares every pane's colors before and after, so the panes using a
        // replaced scheme recolor and nothing else is touched.
        self.reload_schemes()
    }

    /// A credit link, in the system browser; anything but an `https://` link is refused.
    fn open_credit_link(&self, url: &str) {
        if credit_link_ok(url) {
            (self.gallery_io.open)(url);
        } else {
            tracing::warn!(%url, "credit link not opened: not an https link");
        }
    }
}

/// One theme of the demo catalog: neutral sample colors and credit.
fn demo_theme(
    id: &str,
    name: &str,
    version: u32,
    withdrawn: bool,
    author: &str,
    (dark, fg, bg, ansi): (bool, &str, &str, [&str; 16]),
) -> String {
    const KEYS: [&str; 16] = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "bright_black",
        "bright_red",
        "bright_green",
        "bright_yellow",
        "bright_blue",
        "bright_magenta",
        "bright_cyan",
        "bright_white",
    ];
    let variant = if dark { "dark" } else { "light" };
    let mut out = format!(
        "\n[[themes]]\nid = \"{id}\"\nname = \"{name}\"\nversion = {version}\n\
         variant = \"{variant}\"\nwithdrawn = {withdrawn}\n\n[themes.colors]\n\
         foreground = \"{fg}\"\nbackground = \"{bg}\"\ncursor = \"{}\"\nselection = \"{}\"\n",
        ansi[12],
        if dark { ansi[0] } else { ansi[7] }
    );
    for (key, value) in KEYS.iter().zip(ansi) {
        out.push_str(&format!("{key} = \"{value}\"\n"));
    }
    out.push_str(&format!(
        "\n[themes.credit]\nproject = \"Sample Themes\"\nauthor = \"{author}\"\n\
         url = \"https://example.org/sample-themes\"\nlicense = \"MIT\"\n\
         copyright = \"Copyright (c) 2026 Example Author\"\n\
         license_file = \"LICENSES/sample-themes-MIT.txt\"\n\
         description = \"A sample theme.\"\nderived = false\n"
    ));
    out
}

/// The `FBTERM_DEMO_GALLERY` scene: a catalog of sample themes and the installed schemes
/// that give one row of each kind (Install, Installed, Update, and a withdrawn theme still
/// installed), plus a worst-case row with a very long name and credit.
pub fn demo_catalog() -> (Catalog, Vec<Scheme>) {
    let lagoon = (
        true,
        "#d8dee6",
        "#14191f",
        [
            "#262d36", "#e06c6c", "#7cc48a", "#e0b35c", "#6aa6e0", "#b48ee0", "#5cc4c4", "#c8ced6",
            "#5a6470", "#f08a8a", "#98d8a4", "#f0c87a", "#8cbcf0", "#c8a8f0", "#80d8d8", "#eef2f6",
        ],
    );
    let meadow = (
        false,
        "#2e3a2f",
        "#f6f4ec",
        [
            "#3a3a3a", "#b4403c", "#2e6e38", "#7c5c0c", "#2f6aa8", "#8a4aa0", "#1e6c6c", "#d8d4c8",
            "#7a7a72", "#a8423c", "#36763e", "#82620e", "#2c64a0", "#7e4494", "#24706e", "#fbfaf4",
        ],
    );
    let slate = (
        true,
        "#e4e4ea",
        "#1c1d22",
        [
            "#2c2d34", "#d4605c", "#8ab870", "#d8a850", "#7090d0", "#a080c8", "#60a8b8", "#c4c4cc",
            "#5c5e68", "#e47c78", "#a4cc8c", "#e8be6c", "#8ca8e0", "#b89ad8", "#7cc0cc", "#f2f2f6",
        ],
    );
    let dusk = (
        true,
        "#ecdcd0",
        "#221a1e",
        [
            "#342a30", "#e0705c", "#a4b86c", "#e8a85c", "#7c94c8", "#c486b0", "#74b0a8", "#d4c4bc",
            "#66585e", "#f08c78", "#bccc88", "#f4c07c", "#98acd8", "#d4a0c4", "#90c8c0", "#f8ece4",
        ],
    );
    let catalog = |meadow_version: u32, all: bool| {
        let mut text = String::from("format = 1\n");
        let a = "Example Author";
        text.push_str(&demo_theme("lagoon", "Lagoon", 1, false, a, lagoon));
        text.push_str(&demo_theme(
            "meadow",
            "Meadow",
            meadow_version,
            false,
            a,
            meadow,
        ));
        text.push_str(&demo_theme("slate", "Slate", 1, all, a, slate));
        if all {
            text.push_str(&demo_theme("dusk", "Dusk", 1, false, a, dusk));
            // The worst case: a 120-character name and a 400-character author.
            let name = format!("{}Ends Here", "A Very Long Sample Theme Name ".repeat(4));
            let author = "Example Author With A Rather Long Credit, ".repeat(10);
            text.push_str(&demo_theme(
                "long-sample",
                name.trim(),
                1,
                false,
                author.trim_end_matches([',', ' ']),
                slate,
            ));
        }
        parse_catalog(text.as_bytes()).expect("the demo catalog parses")
    };
    let installed = catalog(1, false)
        .themes
        .into_iter()
        .map(|t| t.scheme)
        .collect();
    (catalog(2, true), installed)
}
