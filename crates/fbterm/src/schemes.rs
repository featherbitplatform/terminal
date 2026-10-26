//! Color schemes in the app: which scheme each pane uses, applying it to the pane's terminal
//! when it changes, reloading `schemes/`, and the banner lines for ids that are not found.
//!
//! A pane uses its profile's scheme if that id is known, else the global `color_scheme`
//! setting if known, else Graphite. The colors a pane is drawn with come from its
//! [`App::pane_style`] each frame; the terminal itself keeps a copy only to answer programs
//! asking for its colors (OSC 10/11/4), which [`App::reapply_schemes`] keeps current.

use std::collections::HashMap;
use std::sync::OnceLock;

use fbterm_config::Rgb;
use fbterm_config::schemes::{Scheme, SchemeColors, SchemeSet};
use fbterm_term::ColorScheme;
use iced::Task;

use crate::app::{App, Message};
use crate::layout::PaneId;
use crate::tabs::Pane;

/// Graphite, for a scheme set that somehow lacks it (it never does: built-ins are always in).
fn graphite() -> &'static Scheme {
    static GRAPHITE: OnceLock<Scheme> = OnceLock::new();
    GRAPHITE.get_or_init(|| {
        fbterm_config::schemes::builtins()
            .into_iter()
            .find(|s| s.id == "graphite")
            .unwrap_or_else(|| unreachable!("graphite is a built-in scheme"))
    })
}

/// Load every scheme from `dir`, with each problem as a banner line.
pub(crate) fn load(dir: &std::path::Path) -> (SchemeSet, Vec<String>) {
    let (set, errors) = SchemeSet::load(dir);
    for e in &errors {
        tracing::warn!(error = %e, "scheme file not loaded");
    }
    (set, errors.iter().map(ToString::to_string).collect())
}

/// The first entry of the profile editor's scheme list: no override.
pub(crate) const USE_GLOBAL: &str = "Use global scheme";

/// One entry of a scheme list: a scheme (`id`), or "use the global scheme" (`None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeChoice {
    pub id: Option<String>,
    pub name: String,
}

impl std::fmt::Display for SchemeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

impl App {
    /// The entries of a scheme list, built-ins first (the set's order), read from the current
    /// set each time a view is drawn so a changed `schemes/` shows at once. `with_global`
    /// puts `Use global scheme` first (the profile editor).
    pub(crate) fn scheme_choices(&self, with_global: bool) -> Vec<SchemeChoice> {
        let global = with_global.then(|| SchemeChoice {
            id: None,
            name: USE_GLOBAL.into(),
        });
        global
            .into_iter()
            .chain(self.schemes.all().iter().map(|s| SchemeChoice {
                id: Some(s.id.clone()),
                name: s.name.clone(),
            }))
            .collect()
    }

    /// The scheme `pane` uses: its own if known, else the global one if known, else Graphite.
    pub(crate) fn scheme_for(&self, pane: &Pane) -> &Scheme {
        pane.scheme
            .as_deref()
            .and_then(|id| self.schemes.get(id))
            .or_else(|| self.schemes.get(&self.settings.color_scheme))
            .or_else(|| self.schemes.get("graphite"))
            .unwrap_or_else(|| graphite())
    }

    /// The terminal colors of `pane`'s scheme.
    pub(crate) fn scheme_colors(&self, pane: &Pane) -> ColorScheme {
        ColorScheme::from(&self.scheme_for(pane).colors)
    }

    /// The default background of `pane`'s scheme.
    pub(crate) fn pane_background(&self, pane: &Pane) -> Rgb {
        self.scheme_for(pane).colors.background
    }

    /// Give every open pane `affected` picks its scheme's colors, so programs asking for them
    /// get the new ones. Drawing needs nothing more: the views read the scheme each frame and
    /// redraw with the next message; a font change makes the view measure again and report the
    /// new grid, which reaches the session through the debounced pane resize.
    pub(crate) fn reapply_schemes(&mut self, affected: impl Fn(&Pane) -> bool) -> Task<Message> {
        self.recolor(affected);
        Task::none()
    }

    fn recolor(&self, affected: impl Fn(&Pane) -> bool) {
        let panes = self
            .windows
            .values()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes);
        for pane in panes.filter(|p| affected(p)) {
            if let Some(term) = pane.terminal() {
                self.scheme_colors(pane).apply_to(&mut term.lock());
            }
        }
    }

    /// The colors every open pane uses now, to compare with after a change.
    pub(crate) fn pane_colors(&self) -> HashMap<PaneId, SchemeColors> {
        self.windows
            .values()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|p| (p.id, self.scheme_for(p).colors))
            .collect()
    }

    /// After a change to the schemes, the global setting or the profiles: recolor the panes
    /// whose colors differ from `before`, and update the banner.
    pub(crate) fn schemes_moved(&mut self, before: &HashMap<PaneId, SchemeColors>) {
        let now = self.pane_colors();
        self.recolor(|p| before.get(&p.id) != now.get(&p.id));
        self.refresh_scheme_notices();
    }

    /// Something in `schemes/` changed (on disk, or saved by the app): load the set again and
    /// recolor the panes that use a changed scheme.
    pub(crate) fn reload_schemes(&mut self) -> Task<Message> {
        let (set, errors) = load(&self.paths.schemes());
        if set == self.schemes && errors == self.scheme_errors {
            return Task::none();
        }
        let before = self.pane_colors();
        self.schemes = set;
        self.scheme_errors = errors;
        tracing::info!(schemes = self.schemes.all().len(), "schemes reloaded");
        let now = self.pane_colors();
        let task = self.reapply_schemes(|p| before.get(&p.id) != now.get(&p.id));
        self.refresh_scheme_notices();
        task
    }

    /// What the banner says about schemes: files that did not load, then ids not found.
    fn scheme_warnings(&self) -> Vec<String> {
        let mut out = self.scheme_errors.clone();
        let global = &self.settings.color_scheme;
        if self.schemes.get(global).is_none() {
            out.push(format!(
                "Color scheme \"{global}\" not found; using Graphite."
            ));
        }
        for p in &self.profiles.profiles {
            if let Some(id) = &p.color_scheme
                && self.schemes.get(id).is_none()
            {
                out.push(format!(
                    "Color scheme \"{id}\" not found for profile \"{}\"; using the global scheme.",
                    p.name
                ));
            }
        }
        out
    }

    /// Put the scheme lines in the banner, once: unchanged lines stay as they are (dismissed
    /// or not), changed ones replace them and show the banner again.
    pub(crate) fn refresh_scheme_notices(&mut self) {
        let lines = self.scheme_warnings();
        if lines == self.scheme_notices {
            return;
        }
        let old = std::mem::take(&mut self.scheme_notices);
        self.notices.retain(|n| !old.contains(n));
        if !lines.is_empty() {
            self.notices_dismissed = false;
        }
        self.notices.extend(lines.iter().cloned());
        self.scheme_notices = lines;
    }
}
