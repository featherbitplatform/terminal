//! The right-click menu on a tab (bar or sidebar): `Rename tab` for a terminal tab,
//! `Copy password` for a password SSH profile's tab, and `Close tab`. Only the user's own
//! right-click opens it.

use std::time::Duration;

use fbterm_config::{AuthMethod, ProfileId, ProfileKind};
use fbterm_secrets::{SecretError, SecretKind};
use iced::{Point, Task, window};
use zeroize::Zeroizing;

use crate::app::{App, Delivered, Message};
use crate::context_menu::{ContextMenu, MenuItem};
use crate::keys::Action;
use crate::tabs::TabId;

/// How long a copied password stays on the clipboard before fbterm clears it, if it is
/// still there.
pub(crate) const CLEAR_AFTER: Duration = Duration::from_secs(60);

/// What was read for a copy: the saved password, none, or the storage's refusal.
type Read = Result<Option<Zeroizing<String>>, SecretError>;

/// The tab menu's messages. None carries a password in the clear: the secret rides a
/// [`Delivered`], whose `Debug` says nothing about its value.
#[derive(Clone)]
pub(crate) enum TabMenuMsg {
    /// Copy the saved password of the profile of `tab`'s focused pane: from the menu, and
    /// again after the vault was unlocked for it.
    Copy(window::Id, TabId),
    /// The saved secrets answered (on the blocking pool) for the profile.
    Read(window::Id, ProfileId, Delivered<Read>),
    /// The minute after copy number `u64` passed: look at the clipboard.
    ClearDue(Delivered<Zeroizing<String>>, u64),
    /// The clipboard as read for copy number `u64`'s check.
    ClearCheck(Delivered<Zeroizing<String>>, Option<Zeroizing<String>>, u64),
}

impl std::fmt::Debug for TabMenuMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Copy(w, t) => write!(f, "Copy({w:?}, {t:?})"),
            Self::Read(w, p, _) => write!(f, "Read({w:?}, {p:?}, ..)"),
            Self::ClearDue(..) => write!(f, "ClearDue(..)"),
            Self::ClearCheck(..) => write!(f, "ClearCheck(..)"),
        }
    }
}

/// Whether the clipboard still holds exactly the copied password: only then is it cleared.
pub(crate) fn should_clear(copied: &str, clipboard: Option<&str>) -> bool {
    clipboard == Some(copied)
}

/// Whether copy number `generation`'s check clears: only the latest copy's (`current`), so a
/// second copy within the minute restarts it even for the same password, and only when the
/// clipboard still holds exactly what was copied.
pub(crate) fn clear_now(
    current: u64,
    generation: u64,
    copied: &str,
    clipboard: Option<&str>,
) -> bool {
    generation == current && should_clear(copied, clipboard)
}

impl App {
    pub(crate) fn tab_menu_msg(&mut self, msg: TabMenuMsg) -> Task<Message> {
        match msg {
            TabMenuMsg::Copy(wid, tab) => self.copy_password(wid, tab),
            TabMenuMsg::Read(wid, id, read) => match read.take() {
                Some(read) => self.password_read(wid, id, read),
                None => Task::none(),
            },
            TabMenuMsg::ClearDue(copied, generation) => iced::clipboard::read().map(move |text| {
                let text = text.map(Zeroizing::new);
                Message::TabMenu(TabMenuMsg::ClearCheck(copied.clone(), text, generation))
            }),
            TabMenuMsg::ClearCheck(copied, text, generation) => {
                let current = self.clip_generation;
                let clear = copied.take().is_some_and(|copied| {
                    clear_now(
                        current,
                        generation,
                        &copied,
                        text.as_deref().map(String::as_str),
                    )
                });
                if clear {
                    iced::clipboard::write(String::new())
                } else {
                    Task::none()
                }
            }
        }
    }

    /// `Copy password`: read the saved password of `tab`'s profile on the blocking pool
    /// (never on the UI thread: the vault and the keychain can take their time or ask).
    /// A tab that is gone, or no longer a password SSH profile's, copies nothing.
    pub(crate) fn copy_password(&mut self, wid: window::Id, tab: TabId) -> Task<Message> {
        let Some(id) = self.tab_password_profile(wid, tab) else {
            return Task::none();
        };
        let secrets = self.secrets.clone();
        self.run_blocking(
            move || {
                secrets
                    .get(&id, SecretKind::Password)
                    .map(|found| found.map(Zeroizing::new))
            },
            |e| Err(SecretError::Backend(e)),
            move |read| Message::TabMenu(TabMenuMsg::Read(wid, id, Delivered::new(read))),
        )
    }

    /// What the read gave: the password to the clipboard (and a clear a minute later), or
    /// why not in the window banner. A locked vault asks for its main password first and
    /// copies after the unlock.
    fn password_read(&mut self, wid: window::Id, id: ProfileId, read: Read) -> Task<Message> {
        match read {
            Ok(Some(password)) => {
                self.clip_generation += 1;
                let generation = self.clip_generation;
                let copied = Delivered::new(password.clone());
                let text = password.to_string();
                drop(password);
                Task::batch([
                    iced::clipboard::write(text),
                    Task::perform(async { tokio::time::sleep(CLEAR_AFTER).await }, move |()| {
                        Message::TabMenu(TabMenuMsg::ClearDue(copied.clone(), generation))
                    }),
                ])
            }
            Ok(None) => {
                let name = self
                    .profiles
                    .get(id)
                    .map_or_else(|| id.to_string(), |p| p.name.clone());
                self.show_notice(format!("No saved password for {name}."));
                Task::none()
            }
            Err(SecretError::Locked) => match self.backend.needs_main_password() {
                Some(need) => {
                    let tab = self.windows.get(&wid).and_then(|w| {
                        w.tabs
                            .iter()
                            .find(|t| t.page.is_none() && t.focused_pane().profile == id)
                            .map(|t| t.id)
                    });
                    match tab {
                        Some(tab) => self.ask_main_password(
                            wid,
                            need,
                            Message::TabMenu(TabMenuMsg::Copy(wid, tab)),
                            None,
                        ),
                        None => Task::none(),
                    }
                }
                None => {
                    self.show_notice(SecretError::Locked.to_string());
                    Task::none()
                }
            },
            Err(e) => {
                self.show_notice(e.to_string());
                Task::none()
            }
        }
    }

    /// A right-click on the tab at `index` of window `wid`: its menu at `at`, with
    /// `Copy password` when the tab's focused pane is a password SSH profile's. Not while
    /// something covers the window (see [`App::menu_covered`]). The tab is not selected.
    ///
    /// An item shows its chord only when the key would do the same to this tab, so only on
    /// the active tab (the chords act on it): the copy chord copies the same password, and
    /// the close chord closes the tab only when the tab has one pane (with more it closes
    /// the focused pane).
    pub(crate) fn open_tab_menu(
        &mut self,
        wid: window::Id,
        index: usize,
        at: Point,
    ) -> Task<Message> {
        if self.menu_covered(wid) {
            return Task::none();
        }
        let Some(tab) = self.windows.get(&wid).and_then(|w| w.tabs.get(index)) else {
            return Task::none();
        };
        let revive = self.revive_kind(tab);
        let (tab, panes, rename) = (tab.id, tab.panes.len(), tab.page.is_none());
        let active = self.windows[&wid].active == index;
        let password = self.tab_password_profile(wid, tab).is_some();
        let mut chords = Vec::new();
        if active {
            let shown = [
                (MenuItem::RenameTab, Action::RenameTab, rename),
                (MenuItem::CopyPassword, Action::CopyPassword, password),
                (MenuItem::CloseTab, Action::CloseTab, panes == 1),
            ];
            // The `Reconnect` key brings back every dropped pane of the active tab, as the
            // item does.
            let revive_chord = revive.map(|r| (MenuItem::revive(r), Action::Reconnect, true));
            for (item, action, same) in shown.into_iter().chain(revive_chord) {
                if let Some(label) = self.bindings.label(action).filter(|_| same) {
                    chords.push((item, label));
                }
            }
        }
        self.context_menu = Some(ContextMenu::for_tab(
            wid, tab, at, revive, rename, password, chords,
        ));
        Task::none()
    }

    /// The `Copy password` shortcut: the active tab's, as the tab menu's item would.
    pub(crate) fn copy_active_tab_password(&mut self, wid: window::Id) -> Task<Message> {
        match self.windows.get(&wid).and_then(|w| w.active_tab()) {
            Some(tab) => self.copy_password(wid, tab.id),
            None => Task::none(),
        }
    }

    /// The profile whose password `Copy password` would copy for `tab`: the tab's focused
    /// pane's, when the store still has it and it is an SSH profile with password
    /// authentication.
    pub(crate) fn tab_password_profile(&self, wid: window::Id, tab: TabId) -> Option<ProfileId> {
        let w = self.windows.get(&wid)?;
        let t = w.tabs.iter().find(|t| t.id == tab)?;
        if t.page.is_some() {
            return None;
        }
        let id = t.focused_pane().profile;
        let p = self.profiles.get(id)?;
        match &p.kind {
            ProfileKind::Ssh(s) if s.auth == AuthMethod::Password => Some(id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, SecretStorage, SshProfile};
    use fbterm_secrets::{MemoryStore, SecretBackend, SecretKind, SecretStore};
    use iced::{Point, window};

    use super::{TabMenuMsg, clear_now, should_clear};
    use crate::app::{App, Message, TabBarMsg};
    use crate::context_menu::{MenuItem, MenuMsg};
    use crate::keys::Action;
    use crate::layout::Side;
    use crate::restore_tests::{paths_with, profile};
    use crate::saved_secrets::Purpose;
    use crate::tabs::{Page, Pane, Tab, WindowState};

    pub(crate) fn ssh_password_profile(name: &str) -> Profile {
        Profile {
            icon: None,
            id: ProfileId::new(),
            name: name.into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "h.example.com".into(),
                port: 22,
                user: "root".into(),
                auth: AuthMethod::Password,
                jump: vec![],
                tunnels: vec![],
            }),
        }
    }

    /// An app with one window holding one single-pane tab per profile, in order.
    fn app_with_tabs(known: &[Profile], tabs: &[Profile]) -> (App, window::Id) {
        let (mut app, _) = App::build(paths_with(known), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        for p in tabs {
            w.push(Tab::single(Pane::for_profile(p)));
        }
        w.active = 0;
        app.windows.clear();
        app.windows.insert(wid, w);
        app.sizes.insert(wid, (900.0, 600.0));
        (app, wid)
    }

    #[test]
    fn the_menu_offers_copy_password_only_for_password_ssh_tabs() {
        let (ssh, local) = (ssh_password_profile("prod"), profile("sh"));
        let (mut app, wid) = app_with_tabs(&[ssh.clone(), local.clone()], &[ssh, local]);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .push(Tab::page(Page::Settings));
        app.windows.get_mut(&wid).unwrap().active = 0;
        let at = Point::new(40.0, 10.0);
        let _ = app.open_tab_menu(wid, 0, at);
        let m = app.context_menu.clone().unwrap();
        assert_eq!(
            m.items,
            vec![
                (MenuItem::RenameTab, true),
                (MenuItem::CopyPassword, true),
                (MenuItem::CloseTab, true)
            ]
        );
        assert_eq!(m.selected, MenuItem::RenameTab);
        assert_eq!(m.at, at);
        assert_eq!(
            app.windows[&wid].active, 0,
            "a right-click does not select the tab"
        );
        let _ = app.open_tab_menu(wid, 1, at);
        assert_eq!(
            app.context_menu.clone().unwrap().items,
            vec![(MenuItem::RenameTab, true), (MenuItem::CloseTab, true)]
        );
        // The Settings page tab: never renamed.
        let _ = app.open_tab_menu(wid, 2, at);
        assert_eq!(
            app.context_menu.clone().unwrap().items,
            vec![(MenuItem::CloseTab, true)]
        );
        let _ = app.open_tab_menu(wid, 7, at);
        assert!(
            app.context_menu.is_some(),
            "an unknown index changes nothing"
        );
    }

    /// Two panes side by side in one tab, as a split would make them.
    fn two_panes(first: Pane, second: Pane) -> Tab {
        let (a, b) = (first.id, second.id);
        let mut tab = Tab::single(first);
        tab.layout.split(a, Side::Right, b);
        tab.panes.push(second);
        tab
    }

    #[test]
    fn the_menu_shows_a_chord_only_where_the_key_does_what_the_item_does() {
        let (ssh, local) = (ssh_password_profile("prod"), profile("sh"));
        let (mut app, wid) = app_with_tabs(&[ssh.clone(), local.clone()], &[ssh.clone(), local]);
        let close = app.bindings.label(Action::CloseTab).unwrap();
        let password = app.bindings.label(Action::CopyPassword).unwrap();
        // The active tab with one pane: the chords do exactly what the items do.
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert_eq!(
            m.chord_label(MenuItem::CloseTab).as_deref(),
            Some(close.as_str())
        );
        assert_eq!(
            m.chord_label(MenuItem::CopyPassword).as_deref(),
            Some(password.as_str())
        );
        // A background tab: the chords act on the active tab, so no hint.
        let _ = app.open_tab_menu(wid, 1, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(m.chord_label(MenuItem::CloseTab).is_none());
        app.windows.get_mut(&wid).unwrap().active = 1;
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(m.chord_label(MenuItem::CloseTab).is_none());
        assert!(m.chord_label(MenuItem::CopyPassword).is_none());
        // The active tab split in two: the close chord closes a pane, not the tab; the
        // password one still copies the focused pane's.
        let w = app.windows.get_mut(&wid).unwrap();
        w.tabs[0] = two_panes(Pane::for_profile(&ssh), Pane::for_profile(&ssh));
        w.active = 0;
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(m.chord_label(MenuItem::CloseTab).is_none());
        assert_eq!(
            m.chord_label(MenuItem::CopyPassword).as_deref(),
            Some(password.as_str())
        );
        // An unbound action shows nothing.
        app.bindings = fbterm_config::keys::Bindings::current(
            &[(Action::CloseTab, None)].into_iter().collect(),
        );
        app.windows.get_mut(&wid).unwrap().active = 1;
        let _ = app.open_tab_menu(wid, 1, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(m.chord_label(MenuItem::CloseTab).is_none());
    }

    #[test]
    fn the_copy_password_shortcut_copies_the_active_tabs_password() {
        let (ssh, local) = (ssh_password_profile("prod"), profile("sh"));
        let (mut app, wid) = app_with_tabs(&[ssh.clone(), local.clone()], &[local, ssh.clone()]);
        app.blocking_inline = true;
        app.secrets
            .set(&ssh.id, SecretKind::Password, "s3cret")
            .unwrap();
        // The active tab is the local profile's: nothing to copy, nothing said.
        let _ = app.update(Message::Shortcut(wid, Action::CopyPassword));
        assert_eq!(app.clip_generation, 0);
        assert!(app.visible_notices().is_empty());
        app.windows.get_mut(&wid).unwrap().active = 1;
        let _ = app.update(Message::Shortcut(wid, Action::CopyPassword));
        assert_eq!(app.clip_generation, 1);
    }

    #[test]
    fn a_deleted_profile_offers_no_copy_password() {
        // The pane keeps a ProfileId the store no longer has.
        let ssh = ssh_password_profile("gone");
        let (mut app, wid) = app_with_tabs(&[], &[ssh]);
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        assert_eq!(
            app.context_menu.clone().unwrap().items,
            vec![(MenuItem::RenameTab, true), (MenuItem::CloseTab, true)]
        );
    }

    #[test]
    fn close_tab_closes_the_right_clicked_tab_and_the_menu_follows_its_tab() {
        let (a, b) = (profile("a"), profile("b"));
        let (mut app, wid) = app_with_tabs(&[a.clone(), b.clone()], &[a, b]);
        let _ = app.open_tab_menu(wid, 1, Point::ORIGIN);
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Choose(MenuItem::CloseTab),
        ));
        assert_eq!(app.windows[&wid].tabs.len(), 1);
        assert_eq!(app.windows[&wid].tabs[0].title(), "a");
        assert_eq!(app.windows[&wid].active, 0);
        assert!(app.context_menu.is_none());
        // The menu's tab closed another way: the menu goes with it.
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        assert!(app.context_menu.is_some());
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Close(0)));
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn the_tab_menu_does_not_open_under_the_palette() {
        let a = profile("a");
        let (mut app, wid) = app_with_tabs(std::slice::from_ref(&a), std::slice::from_ref(&a));
        let _ = app.open_palette(wid, String::new());
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn should_clear_only_an_exact_match() {
        assert!(should_clear("s3cret", Some("s3cret")));
        assert!(!should_clear("s3cret", Some("s3cret ")));
        assert!(!should_clear("s3cret", Some("")));
        assert!(!should_clear("s3cret", None));
        // The first copy's check after a second copy finds the second value: left alone.
        assert!(!should_clear("first", Some("second")));
    }

    #[test]
    fn a_second_copy_restarts_the_minute_even_for_the_same_password() {
        // Only the latest copy's check may clear, and only an exact match.
        assert!(clear_now(2, 2, "p", Some("p")));
        assert!(!clear_now(2, 1, "p", Some("p")));
        assert!(!clear_now(2, 2, "p", Some("q")));
        let ssh = ssh_password_profile("prod");
        let (mut app, wid) = app_with_tabs(std::slice::from_ref(&ssh), std::slice::from_ref(&ssh));
        app.blocking_inline = true;
        app.secrets
            .set(&ssh.id, SecretKind::Password, "s3cret")
            .unwrap();
        let tab = app.windows[&wid].tabs[0].id;
        assert_eq!(app.clip_generation, 0);
        let _ = app.update(Message::TabMenu(TabMenuMsg::Copy(wid, tab)));
        let _ = app.update(Message::TabMenu(TabMenuMsg::Copy(wid, tab)));
        assert_eq!(app.clip_generation, 2);
    }

    #[test]
    fn messages_never_print_the_password_or_its_length() {
        use zeroize::Zeroizing;
        let m = TabMenuMsg::ClearCheck(
            crate::app::Delivered::new(Zeroizing::new("s3cret".to_string())),
            Some(Zeroizing::new("s3cret".to_string())),
            1,
        );
        assert_eq!(format!("{m:?}"), "ClearCheck(..)");
        let m = TabMenuMsg::ClearDue(
            crate::app::Delivered::new(Zeroizing::new("s3cret".to_string())),
            1,
        );
        assert_eq!(format!("{m:?}"), "ClearDue(..)");
    }

    #[test]
    fn a_missing_password_says_so_in_the_banner_and_a_saved_one_does_not() {
        let ssh = ssh_password_profile("prod");
        let (mut app, wid) = app_with_tabs(std::slice::from_ref(&ssh), std::slice::from_ref(&ssh));
        app.blocking_inline = true;
        let tab = app.windows[&wid].tabs[0].id;
        let _ = app.update(Message::TabMenu(TabMenuMsg::Copy(wid, tab)));
        assert!(
            app.visible_notices()
                .contains(&"No saved password for prod.".to_string()),
            "{:?}",
            app.visible_notices()
        );
        app.secrets
            .set(&ssh.id, SecretKind::Password, "s3cret")
            .unwrap();
        app.notices.clear();
        let _ = app.update(Message::TabMenu(TabMenuMsg::Copy(wid, tab)));
        assert!(app.visible_notices().is_empty());
        // A tab that is gone copies nothing and says nothing.
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Close(0)));
        let _ = app.update(Message::TabMenu(TabMenuMsg::Copy(wid, tab)));
        assert!(app.visible_notices().is_empty());
    }

    #[test]
    fn choosing_copy_password_from_the_menu_closes_it_and_runs_the_copy() {
        let ssh = ssh_password_profile("prod");
        let (mut app, wid) = app_with_tabs(std::slice::from_ref(&ssh), std::slice::from_ref(&ssh));
        app.blocking_inline = true;
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Choose(MenuItem::CopyPassword),
        ));
        assert!(app.context_menu.is_none());
        assert!(
            app.visible_notices()
                .contains(&"No saved password for prod.".to_string())
        );
    }

    #[test]
    fn a_locked_vault_asks_for_the_main_password_then_copies() {
        let ssh = ssh_password_profile("prod");
        let (mut app, wid) = app_with_tabs(std::slice::from_ref(&ssh), std::slice::from_ref(&ssh));
        app.blocking_inline = true;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.fbvault");
        let setup = SecretBackend::with_system(
            SecretStorage::Vault,
            Box::new(MemoryStore::new()),
            path.clone(),
        );
        setup.create_vault("main").unwrap();
        setup.set(&ssh.id, SecretKind::Password, "s3cret").unwrap();
        // A fresh backend on the same file: the vault is locked.
        let locked = Arc::new(SecretBackend::with_system(
            SecretStorage::Vault,
            Box::new(MemoryStore::new()),
            path,
        ));
        app.use_secret_backend(locked);
        let tab = app.windows[&wid].tabs[0].id;
        let _ = app.update(Message::TabMenu(TabMenuMsg::Copy(wid, tab)));
        let d = app.vault.dialog.as_ref().expect("the unlock dialog");
        assert_eq!(d.window, wid);
        match &d.purpose {
            Purpose::Then { then, on_cancel } => {
                assert!(
                    matches!(**then, Message::TabMenu(TabMenuMsg::Copy(w, t)) if w == wid && t == tab)
                );
                assert!(on_cancel.is_none());
            }
            _ => panic!("not a Then purpose"),
        }
        assert!(app.visible_notices().is_empty());
    }

    #[test]
    fn a_right_click_message_opens_the_menu_through_the_tab_bar() {
        let a = profile("a");
        let (mut app, wid) = app_with_tabs(std::slice::from_ref(&a), std::slice::from_ref(&a));
        let at = Point::new(30.0, 12.0);
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Menu { index: 0, at }));
        assert_eq!(app.context_menu.as_ref().map(|m| m.at), Some(at));
    }
}
