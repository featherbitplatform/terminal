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
use fbterm_config::schemes::{Scheme, new_user_id, save_user};
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
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "shown by the gallery view, which comes next")
    )]
    Failed(String),
}

/// The answer to an Update (or Install) over a scheme the user edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "answered in the gallery view, which comes next")
)]
pub enum UpdateChoice {
    /// Replace my changes.
    Replace,
    /// Install as a copy: a new user scheme with the new colors and the credit.
    Copy,
    Cancel,
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
