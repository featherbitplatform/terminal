//! What application actions do (shortcuts, tab-bar buttons and the palette), and the palette.

use iced::{Task, window};

use crate::app::{App, Message};
use crate::editor::EditorMsg;
use crate::keys::Action;
use crate::layout::{Dir, Side};
use crate::palette::{self, PaletteItem, PaletteMsg, PaletteState, Pick, Placing, Stage};
use crate::pane_actions::Zoom;
use crate::settings_window::SettingsMsg;
use crate::tabs::{TabId, WindowState};

impl App {
    pub(crate) fn action(&mut self, window: Option<window::Id>, action: Action) -> Task<Message> {
        if let Some(wid) = window {
            self.focused = Some(wid);
        }
        let wid = self.focused_window();
        match action {
            // New tab asks which profile to open: the palette, with nothing preselected.
            Action::NewTab => match wid {
                Some(wid) => self.open_palette(wid, String::new()),
                None => self.open_window(self.default_profile()),
            },
            // The focused pane; the last pane of a tab closes the tab.
            Action::CloseTab => self.on_window(wid, App::close_focused_pane),
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
            Action::NextTab => self.with_focused(WindowState::next),
            Action::PrevTab => self.with_focused(WindowState::prev),
            Action::SelectTab(n) => self.with_focused(|w| w.select_number(n)),
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
            Action::Settings => self.settings_msg(SettingsMsg::Open),
            Action::EditSchemes => self.open_schemes(None),
            Action::Tunnels => self.open_tunnels(),
            Action::FilePanel => self.on_window(wid, App::toggle_file_panel),
            Action::OpenSettingsFile => {
                crate::config_files::open_settings_file(&self.paths);
                Task::none()
            }
            Action::RevealConfigFolder => {
                crate::config_files::reveal_config_folder(&self.paths);
                Task::none()
            }
            Action::ExportTab => self.export_tab(wid),
            Action::Quit => self.quit(),
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
            crate::tunnels_window::palette_rows(&self.profiles, &self.tunnels),
        )
    }

    /// Open launchers list the profiles and tunnels as they are now (keeping their query).
    pub(crate) fn refresh_palettes(&mut self) {
        if self.palettes.is_empty() {
            return;
        }
        let items = self.palette_items();
        for state in self.palettes.values_mut() {
            state.set_items(items.clone(), &self.recency);
        }
    }

    /// Open `wid`'s palette (or reset it) with `query`, and focus its input.
    pub(crate) fn open_palette(&mut self, wid: window::Id, query: String) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        let state = PaletteState::new(self.palette_items(), query, &self.recency);
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
        if tab.panes.len() < 2 {
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
            // From the palette, "New tab" is the default shell (the palette itself just closed).
            Action::NewTab => match self.default_profile() {
                Some(profile) => self.open_tab(wid, profile),
                None => Task::none(),
            },
            other => self.action(Some(wid), other),
        }
    }
}
