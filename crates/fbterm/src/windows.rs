//! Opening, restoring and closing windows and tabs, quitting, and saving the layout.

use std::time::Duration;

use fbterm_config::{ProfileId, SavedWindow};
use iced::{Task, window};

use crate::app::{App, Message, unix_now};
use crate::chrome::Chrome;
use crate::restore;
use crate::tabs::{self, Pane, Tab, TabState, WindowState};

impl App {
    /// A new terminal window of `size` (logical pixels), with no tabs yet.
    pub(crate) fn new_window(&mut self, size: (f32, f32)) -> (window::Id, Task<Message>) {
        let chrome = Chrome::current();
        let backdrop = chrome.backdrop(
            self.settings.background_opacity,
            self.settings.background_blur,
        );
        let (id, open) = window::open(chrome.window_settings(window::Settings {
            size: iced::Size::new(size.0, size.1),
            min_size: Some(iced::Size::new(360.0, 200.0)),
            exit_on_close_request: false,
            transparent: backdrop.transparent,
            blur: backdrop.blur,
            ..window::Settings::default()
        }));
        self.windows.insert(id, WindowState::new());
        self.sizes.insert(id, size);
        (id, open.map(Message::WindowOpened))
    }

    /// Reopen the saved windows with their tabs not connected, then connect the active tab of
    /// each; the others connect when first selected (see [`App::shown`]). With nothing to
    /// restore, the default single window opens.
    pub(crate) fn restore_windows(&mut self, plan: &[SavedWindow]) -> Task<Message> {
        if plan.is_empty() {
            return self.open_window(self.default_profile());
        }
        let mut tasks = Vec::new();
        for saved in plan {
            let (wid, open) = self.new_window(saved.size);
            let window = self.windows.get_mut(&wid).expect("just inserted");
            for tab in &saved.tabs {
                if let Some(tab) = restore::build_tab(tab, &self.profiles) {
                    window.tabs.push(tab);
                }
            }
            window.active = saved.active.min(window.tabs.len().saturating_sub(1));
            tracing::info!(
                tabs = window.tabs.len(),
                active = window.active,
                "restoring window"
            );
            tasks.push(open);
            tasks.push(self.shown(wid));
        }
        Task::batch(tasks)
    }

    pub fn open_window(&mut self, profile: Option<ProfileId>) -> Task<Message> {
        let (id, open) = self.new_window(restore::DEFAULT_SIZE);
        let start = match profile {
            Some(p) => self.open_tab(id, p),
            None => Task::none(),
        };
        open.chain(start)
    }

    pub fn open_tab(&mut self, wid: window::Id, profile: ProfileId) -> Task<Message> {
        let Some(p) = self.profiles.get(profile) else {
            return Task::none();
        };
        let tab = Tab::single(Pane::for_profile(p));
        self.recency.bump(profile, unix_now());
        let id = tab.focused;
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        window.push(tab);
        self.save_state();
        self.start(wid, id)
    }

    /// The tab's close button: a tab with several live panes asks first (`Dialog::CloseTab`),
    /// anything else closes at once. The pane shortcut and bulk closes call `close_tab` directly.
    pub(crate) fn close_tab_asking(&mut self, wid: window::Id, index: usize) -> Task<Message> {
        let Some(tab) = self.windows.get(&wid).and_then(|w| w.tabs.get(index)) else {
            return Task::none();
        };
        if tab.live_panes() < 2 {
            return self.close_tab(wid, index);
        }
        let (id, panes) = (tab.id, tab.panes.len());
        if !self.prompts.has_close(wid, id) {
            self.prompts.push(
                wid,
                None,
                crate::prompts::Dialog::CloseTab { tab: id, panes },
            );
        }
        Task::none()
    }

    pub(crate) fn close_tab(&mut self, wid: window::Id, index: usize) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        let Some(closed) = window.close(index) else {
            return Task::none();
        };
        let freed = crate::memory::weak_terminals(closed.tab.panes.iter());
        let gone: Vec<_> = closed.tab.panes.iter().map(|p| p.id).collect();
        self.forget_panes(&gone);
        drop(closed.tab);
        self.save_state();
        self.release_closed(freed);
        if closed.window_empty {
            self.close_window(wid)
        } else {
            self.shown(wid)
        }
    }

    /// A tab or window the user closed freed `terminals`: request the memory release once none
    /// is held any more. The session task may still hold its reference for a moment (or, with a
    /// background job on the PTY, for long), so a held terminal waits in `pending_release` for
    /// `SessionEvent::Released` (see `release_swept`); nothing polls. Only user closes come
    /// here, never output or the end of a program.
    pub(crate) fn release_closed(&mut self, terminals: Vec<crate::memory::WeakTerminal>) {
        if terminals.is_empty() {
            return;
        }
        if terminals.iter().all(|t| t.strong_count() == 0) {
            crate::memory::release_freed();
        } else {
            self.pending_release.push(terminals);
        }
    }

    /// A closed session task let go of its terminal: request the release if that freed the
    /// last terminal of a tab the user closed (one request however many groups finished).
    pub(crate) fn release_swept(&mut self) {
        let before = self.pending_release.len();
        self.pending_release
            .retain(|g| g.iter().any(|t| t.strong_count() > 0));
        if self.pending_release.len() < before {
            crate::memory::release_freed();
        }
    }

    /// The active tab of `wid` was just shown (selected, created, or its window focused): take
    /// the redraws its sessions left pending while hidden (the redraw follows this update), and
    /// start every pane that never was (restored tabs connect on first focus).
    pub(crate) fn shown(&mut self, wid: window::Id) -> Task<Message> {
        let Some(window) = self.windows.get(&wid) else {
            return Task::none();
        };
        tabs::on_shown(window);
        let waiting: Vec<_> = window
            .active_tab()
            .map(|t| {
                t.panes
                    .iter()
                    .filter(|p| p.state == TabState::NotConnected)
                    .map(|p| p.id)
                    .collect()
            })
            .unwrap_or_default();
        Task::batch(
            waiting
                .into_iter()
                .map(|id| self.start(wid, id))
                .collect::<Vec<_>>(),
        )
    }

    /// Write `state.toml`: windows, tabs, selection, sizes and recency. Does nothing for debug
    /// runs (see `state_saving_enabled`) and while `profiles.toml` is unreadable (the tabs would
    /// look orphaned; the file keeps the layout until the profiles load again).
    pub(crate) fn save_state(&self) {
        if !self.state_saving_enabled || self.quitting || self.profiles_error.is_some() {
            return;
        }
        let state = restore::snapshot(&self.windows, &self.sizes, &self.recency);
        if let Err(e) = state.save(&self.paths.state()) {
            tracing::warn!(error = %e, "could not save the session state");
        }
    }

    /// Close a window on purpose (its `Closed` event then is not an OS teardown).
    pub(crate) fn close_window(&mut self, id: window::Id) -> Task<Message> {
        self.closing.insert(id);
        window::close(id)
    }

    /// How windows close and the app quits, as far as `state.toml` is concerned:
    /// - Quit (Cmd+Q / Ctrl+Shift+Q / palette) saves once while every window still exists, then
    ///   closes them all and exits; nothing saves after that, so every window comes back.
    /// - The close button on the last terminal window is Quit. On any other terminal window it
    ///   drops that window from the layout. Closing the last tab of the last window leaves no
    ///   tabs to save, so the next launch opens the default window.
    /// - macOS Cmd+Q from the app menu is `terminate:`: winit closes the windows without a
    ///   close request and the process ends; windows closing unasked count as a quit, and
    ///   state.toml is already current because it is saved on every layout change.
    pub(crate) fn quit(&mut self) -> Task<Message> {
        if self.quitting {
            return Task::none();
        }
        // Settings changes still waiting for their quiet period are written now.
        self.write_pending_settings();
        self.save_state();
        self.quitting = true;
        let ids: Vec<_> = self
            .windows
            .keys()
            .copied()
            .chain(self.editor_window())
            .chain(self.settings_window())
            .collect();
        self.closing.extend(ids.iter().copied());
        Task::batch(ids.into_iter().map(window::close)).chain(iced::exit())
    }

    pub(crate) fn close_requested(&mut self, id: window::Id) -> Task<Message> {
        if self.settings_window() == Some(id) {
            self.write_pending_settings();
        }
        if self.editor_window() == Some(id)
            || self.settings_window() == Some(id)
            || self.windows.len() > 1
        {
            self.close_window(id)
        } else {
            self.quit()
        }
    }

    pub(crate) fn window_resized(&mut self, id: window::Id, size: iced::Size) -> Task<Message> {
        if !self.windows.contains_key(&id) {
            return Task::none();
        }
        self.sizes.insert(id, (size.width, size.height));
        // Dragging a window edge resizes continuously: save once it settles.
        self.save_state_soon()
    }

    /// Save after 500 ms without another request (window edge and divider drags).
    pub(crate) fn save_state_soon(&mut self) -> Task<Message> {
        self.resize_count += 1;
        let n = self.resize_count;
        Task::perform(
            async { tokio::time::sleep(Duration::from_millis(500)).await },
            move |()| Message::SaveState(n),
        )
    }
}
