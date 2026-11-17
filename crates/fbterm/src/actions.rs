//! What application actions do (shortcuts, tab-bar buttons and the palette), and the palette.

use iced::{Task, window};

use crate::app::{App, Message};
use crate::editor::EditorMsg;
use crate::keys::Action;
use crate::layout::{Dir, Side};
use crate::palette::{self, PaletteItem, PaletteMsg, PaletteState, Pick, Placing, Stage};
use crate::pane_actions::Zoom;
use crate::settings_window::SettingsMsg;
use crate::tab_groups::Step;
use crate::tabs::{TabId, WindowState};

impl App {
    pub(crate) fn action(&mut self, window: Option<window::Id>, action: Action) -> Task<Message> {
        if let Some(wid) = window {
            self.focused = Some(wid);
        }
        let wid = self.focused_window();
        if needs_terminal(action) && !terminal_active(wid.and_then(|wid| self.windows.get(&wid))) {
            return Task::none();
        }
        match action {
            // New tab asks which profile to open: the palette, with nothing preselected.
            Action::NewTab => match wid {
                Some(wid) => self.open_palette(wid, String::new()),
                None => self.open_window(self.default_profile()),
            },
            // The focused pane; the last pane of a tab closes the tab.
            Action::CloseTab => self.on_window(wid, App::close_focused_pane),
            Action::CopyPassword => self.on_window(wid, App::copy_active_tab_password),
            Action::RenameTab => self.on_window(wid, App::rename_active_tab),
            Action::Reconnect => self.on_window(wid, App::revive_active_tab),
            Action::SplitRight => self.on_window(wid, |app, wid| app.split_pane(wid, Side::Right)),
            Action::SplitDown => self.on_window(wid, |app, wid| app.split_pane(wid, Side::Bottom)),
            Action::FocusLeft => {
                self.on_window(wid, |app, wid| app.focus_direction(wid, Dir::Left))
            }
            Action::FocusRight => {
                self.on_window(wid, |app, wid| app.focus_direction(wid, Dir::Right))
            }
            Action::FocusUp => self.on_window(wid, |app, wid| app.focus_direction(wid, Dir::Up)),
            Action::FocusDown => {
                self.on_window(wid, |app, wid| app.focus_direction(wid, Dir::Down))
            }
            Action::ZoomIn => self.on_window(wid, |app, wid| app.zoom_pane(wid, Zoom::In)),
            Action::ZoomOut => self.on_window(wid, |app, wid| app.zoom_pane(wid, Zoom::Out)),
            Action::ZoomReset => self.on_window(wid, |app, wid| app.zoom_pane(wid, Zoom::Reset)),
            Action::MaximizePane => self.on_window(wid, App::toggle_maximize),
            Action::FindInTab => self.on_window(wid, App::find_in_tab),
            Action::MergeTab => self.on_window(wid, App::merge_tab_palette),
            Action::MoveTile => self.on_window(wid, App::move_tile_palette),
            Action::TileToTab => self.on_window(wid, |app, wid| {
                let Some(w) = app.windows.get(&wid) else {
                    return Task::none();
                };
                let (end, pane) = (w.tabs.len(), w.active_tab().map(|t| t.focused));
                match pane {
                    Some(pane) => app.tile_to_tab(wid, pane, end),
                    None => Task::none(),
                }
            }),
            // Over the rows that can be seen: a folded group is skipped (`tab_groups.rs`).
            Action::NextTab => self.travel_tabs(wid, Step::Next),
            Action::PrevTab => self.travel_tabs(wid, Step::Prev),
            Action::SelectTab(n) => self.travel_tabs(wid, Step::Number(n)),
            Action::Palette => match wid {
                Some(wid) if self.palettes.contains_key(&wid) => {
                    self.palette(wid, PaletteMsg::Close)
                }
                Some(wid) => {
                    self.close_find_all();
                    self.palette(wid, PaletteMsg::Open)
                }
                None => Task::none(),
            },
            Action::NewWindow => self.open_window(self.default_profile()),
            Action::EditProfiles => self.editor_msg(EditorMsg::Open),
            Action::DuplicateProfile => self.duplicate_action(),
            Action::Settings => self.settings_msg(SettingsMsg::Open),
            Action::EditSchemes => self.open_schemes(None),
            Action::Tunnels => self.tunnels_action(window),
            Action::ExportProfiles => self.open_archive(crate::archive_window::ArchiveMode::Export),
            Action::ImportProfiles => self.open_archive(crate::archive_window::ArchiveMode::Import),
            Action::FilePanel => self.on_window(wid, App::toggle_file_panel),
            Action::VerticalTabs => self.toggle_vertical_tabs(),
            Action::OpenSettingsFile => {
                crate::config_files::open_settings_file(&self.paths);
                Task::none()
            }
            Action::RevealConfigFolder => {
                crate::config_files::reveal_config_folder(&self.paths);
                Task::none()
            }
            Action::ExportTab => self.export_tab(wid),
            Action::Quit => self.quit_asking(),
            Action::Find => match wid {
                Some(wid) => self.find(wid, crate::find::FindMsg::Open),
                None => Task::none(),
            },
            Action::FindAll => match wid {
                Some(wid) => self.find_all(wid, crate::find::all::FindAllMsg::Open),
                None => Task::none(),
            },
            Action::FindNext => match wid {
                Some(wid) => self.find(wid, crate::find::FindMsg::Next),
                None => Task::none(),
            },
            Action::FindPrevious => match wid {
                Some(wid) => self.find(wid, crate::find::FindMsg::Previous),
                None => Task::none(),
            },
        }
    }

    /// Run `f` on the window an action is for, if there is one.
    fn on_window(
        &mut self,
        wid: Option<window::Id>,
        f: impl FnOnce(&mut Self, window::Id) -> Task<Message>,
    ) -> Task<Message> {
        match wid {
            Some(wid) => f(self, wid),
            None => Task::none(),
        }
    }

    /// What the launcher lists: the profiles, the tunnels with their states, the actions.
    pub(crate) fn palette_items(&self) -> Vec<PaletteItem> {
        palette::items(
            &self.profiles,
            crate::tunnels_panel::palette_rows(&self.profiles, &self.tunnels),
        )
    }

    /// Open launchers list the profiles and tunnels as they are now (keeping their query).
    pub(crate) fn refresh_palettes(&mut self) {
        if self.palettes.is_empty() {
            return;
        }
        let items = self.palette_items();
        let windows = &self.windows;
        for (wid, state) in self.palettes.iter_mut() {
            let window = windows.get(wid);
            state.set_items(
                items_for(window, items.clone()),
                terminal_active(window),
                &self.recency,
            );
        }
    }

    /// An open launcher follows its window's active tab: switching to or from a page tab
    /// (with the palette still up) adds or drops the rows that need a terminal.
    pub(crate) fn sync_palette_items(&mut self) {
        let stale = self.palettes.iter().any(|(wid, state)| {
            state.stage == palette::Stage::Launch
                && state.for_terminal() != terminal_active(self.windows.get(wid))
        });
        if stale {
            self.refresh_palettes();
        }
    }

    /// Open `wid`'s palette (or reset it) with `query`, and focus its input.
    pub(crate) fn open_palette(&mut self, wid: window::Id, query: String) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        let window = self.windows.get(&wid);
        let items = items_for(window, self.palette_items());
        let state = PaletteState::new(items, query, terminal_active(window), &self.recency);
        let input = state.input.clone();
        self.palettes.insert(wid, state);
        iced::widget::operation::focus(input)
    }

    pub(crate) fn palette(&mut self, wid: window::Id, msg: PaletteMsg) -> Task<Message> {
        let recency = &self.recency;
        let Some(state) = self.palettes.get_mut(&wid) else {
            return match msg {
                PaletteMsg::Open => self.open_palette(wid, String::new()),
                _ => Task::none(),
            };
        };
        let chosen = match msg {
            PaletteMsg::Open => return self.open_palette(wid, String::new()),
            PaletteMsg::Query(q) => {
                state.set_query(q, recency);
                return Task::none();
            }
            PaletteMsg::Up => {
                state.up();
                return Task::none();
            }
            PaletteMsg::Down => {
                state.down();
                return Task::none();
            }
            PaletteMsg::Close => None,
            PaletteMsg::Submit { .. } => state.selected_item().cloned(),
            PaletteMsg::Pick(position) => state.at(position).cloned(),
        };
        if matches!(msg, PaletteMsg::Submit { .. } | PaletteMsg::Pick(_)) && chosen.is_none() {
            // Nothing matches: Enter does nothing, the palette stays open.
            return Task::none();
        }
        let Some(stage) = self.palettes.remove(&wid).map(|s| s.stage) else {
            return Task::none();
        };
        match chosen {
            Some(PaletteItem::Profile { id, .. })
                if msg == (PaletteMsg::Submit { split: true }) =>
            {
                self.open_in_split(wid, id, Side::Right)
            }
            Some(PaletteItem::Profile { id, .. }) => self.open_tab(wid, id),
            Some(PaletteItem::Tunnel { key, .. }) => self.tunnel_toggle(&key),
            Some(PaletteItem::Action { action, .. }) => self.run_palette_action(wid, action),
            Some(PaletteItem::Choice { pick, .. }) => self.palette_choice(wid, stage, pick),
            None => Task::none(),
        }
    }

    /// Open `wid`'s palette on a rearranging step listing `items` (nothing to list: nothing
    /// opens).
    fn open_stage(
        &mut self,
        wid: window::Id,
        stage: Stage,
        items: Vec<PaletteItem>,
    ) -> Task<Message> {
        if items.is_empty() {
            return Task::none();
        }
        self.close_find_all();
        let state = PaletteState::staged(stage, items);
        let input = state.input.clone();
        self.palettes.insert(wid, state);
        iced::widget::operation::focus(input)
    }

    /// "Merge tab into…": first which of the window's other tabs the active one goes into.
    fn merge_tab_palette(&mut self, wid: window::Id) -> Task<Message> {
        let Some(w) = self.windows.get(&wid) else {
            return Task::none();
        };
        let Some(source) = w.active_tab().map(|t| t.id) else {
            return Task::none();
        };
        // Only the tabs it fits into somewhere.
        let items = palette::tab_choices(w, source)
            .into_iter()
            .filter(|item| match item {
                PaletteItem::Choice {
                    pick: Pick::Tab(into),
                    ..
                } => !self.merge_choices(wid, source, *into).is_empty(),
                _ => false,
            })
            .collect();
        self.open_stage(wid, Stage::MergeInto { source }, items)
    }

    /// Where tab `source` can go in tab `into`: the drop choices that keep every pane at least
    /// the minimum size.
    fn merge_choices(&self, wid: window::Id, source: TabId, into: TabId) -> Vec<PaletteItem> {
        let Some(tab) = self
            .windows
            .get(&wid)
            .and_then(|w| w.find(into).map(|i| &w.tabs[i]))
        else {
            return Vec::new();
        };
        palette::drop_choices(tab, None)
            .into_iter()
            .filter(|item| match item {
                PaletteItem::Choice {
                    pick: Pick::Drop(at),
                    ..
                } => self.merged_layout(wid, source, into, *at).is_some(),
                _ => false,
            })
            .collect()
    }

    /// "Move tile…": where the focused pane goes within its tab.
    fn move_tile_palette(&mut self, wid: window::Id) -> Task<Message> {
        let Some(tab) = self.windows.get(&wid).and_then(WindowState::active_tab) else {
            return Task::none();
        };
        if tab.page.is_some() || tab.panes.len() < 2 {
            return Task::none();
        }
        let (pane, into) = (tab.focused, tab.id);
        // Only the moves that keep every pane at least the minimum size.
        let items = palette::drop_choices(tab, Some(pane))
            .into_iter()
            .filter(|item| match item {
                PaletteItem::Choice {
                    pick: Pick::Drop(at),
                    ..
                } => self.moved_layout(wid, pane, *at).is_some(),
                _ => false,
            })
            .collect();
        self.open_stage(
            wid,
            Stage::Place {
                what: Placing::Tile(pane),
                into,
            },
            items,
        )
    }

    /// A rearranging choice was picked at `stage`.
    fn palette_choice(&mut self, wid: window::Id, stage: Stage, pick: Pick) -> Task<Message> {
        match (stage, pick) {
            (Stage::MergeInto { source }, Pick::Tab(into)) => {
                let items = self.merge_choices(wid, source, into);
                let what = Placing::Tab(source);
                self.open_stage(wid, Stage::Place { what, into }, items)
            }
            (
                Stage::Place {
                    what: Placing::Tab(source),
                    into,
                },
                Pick::Drop(at),
            ) => self.merge_tab(wid, source, into, at),
            (
                Stage::Place {
                    what: Placing::Tile(pane),
                    ..
                },
                Pick::Drop(at),
            ) => self.move_tile(wid, pane, at),
            _ => Task::none(),
        }
    }

    pub(crate) fn run_palette_action(&mut self, wid: window::Id, action: Action) -> Task<Message> {
        match action {
            // From the palette, "New tab" is the default shell (the palette itself just closed);
            // with no local profile to start, the Profiles tab is opened.
            Action::NewTab => match self.default_profile() {
                Some(profile) => self.open_tab(wid, profile),
                None => self.open_editor(),
            },
            other => self.action(Some(wid), other),
        }
    }
}

/// The actions that work on a terminal pane or the tabs holding them: a page tab's one pane
/// is a stand-in with no terminal, so on a page tab they do nothing and the launcher does not
/// list them.
fn needs_terminal(action: Action) -> bool {
    match action {
        Action::Find
        | Action::FindInTab
        | Action::FindNext
        | Action::FindPrevious
        | Action::ZoomIn
        | Action::ZoomOut
        | Action::ZoomReset
        | Action::MaximizePane
        | Action::MergeTab
        | Action::MoveTile
        | Action::TileToTab
        | Action::SplitRight
        | Action::SplitDown
        | Action::RenameTab
        | Action::Reconnect
        | Action::ExportTab => true,
        Action::NewTab
        | Action::CloseTab
        | Action::NextTab
        | Action::PrevTab
        | Action::SelectTab(_)
        | Action::Palette
        | Action::NewWindow
        | Action::Quit
        | Action::EditProfiles
        | Action::DuplicateProfile
        | Action::Settings
        | Action::EditSchemes
        | Action::Tunnels
        | Action::ExportProfiles
        | Action::ImportProfiles
        | Action::FilePanel
        | Action::OpenSettingsFile
        | Action::RevealConfigFolder
        | Action::FindAll
        | Action::FocusLeft
        | Action::FocusRight
        | Action::FocusUp
        | Action::FocusDown
        | Action::VerticalTabs
        | Action::CopyPassword => false,
    }
}

/// Whether `window`'s active tab holds a terminal (not a page tab).
fn terminal_active(window: Option<&WindowState>) -> bool {
    !window
        .and_then(WindowState::active_tab)
        .is_some_and(|t| t.page.is_some())
}

/// The launcher rows for `window`.
fn items_for(window: Option<&WindowState>, mut items: Vec<PaletteItem>) -> Vec<PaletteItem> {
    if !terminal_active(window) {
        items.retain(
            |i| !matches!(i, PaletteItem::Action { action, .. } if needs_terminal(*action)),
        );
    }
    items
}
