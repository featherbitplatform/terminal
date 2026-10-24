//! What application actions do (shortcuts, tab-bar buttons and the palette), and the palette.

use iced::{Task, window};

use crate::app::{App, Message};
use crate::editor::EditorMsg;
use crate::keys::Action;
use crate::palette::{self, PaletteItem, PaletteMsg, PaletteState};
use crate::settings_window::SettingsMsg;
use crate::tabs::WindowState;

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
            Action::CloseTab => match wid {
                Some(wid) => {
                    let active = self.windows.get(&wid).map_or(0, |w| w.active);
                    self.close_tab(wid, active)
                }
                None => Task::none(),
            },
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
            Action::OpenSettingsFile => {
                crate::config_files::open_settings_file(&self.paths);
                Task::none()
            }
            Action::RevealConfigFolder => {
                crate::config_files::reveal_config_folder(&self.paths);
                Task::none()
            }
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

    /// Open `wid`'s palette (or reset it) with `query`, and focus its input.
    pub(crate) fn open_palette(&mut self, wid: window::Id, query: String) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        let state = PaletteState::new(palette::items(&self.profiles), query, &self.recency);
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
            PaletteMsg::Submit => state.selected_item().cloned(),
            PaletteMsg::Pick(position) => state.at(position).cloned(),
        };
        if matches!(msg, PaletteMsg::Submit | PaletteMsg::Pick(_)) && chosen.is_none() {
            // Nothing matches: Enter does nothing, the palette stays open.
            return Task::none();
        }
        self.palettes.remove(&wid);
        match chosen {
            Some(PaletteItem::Profile { id, .. }) => self.open_tab(wid, id),
            Some(PaletteItem::Action { action, .. }) => self.run_palette_action(wid, action),
            None => Task::none(),
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
