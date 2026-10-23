//! Debug-build aids that set up a scene at startup (see the list in `main.rs`).

use fbterm_config::ProfileId;
use fbterm_secrets::SecretKind;
use iced::Task;

use crate::app::{App, Message, debug_env};
use crate::editor::EditorMsg;
use crate::prompts::{Ask, Dialog};

impl App {
    /// `FBTERM_OPEN_TABS=name,name` (debug aid until restore exists): extra tabs opened in the
    /// first window at startup, by profile name.
    pub(crate) fn open_debug_tabs(&mut self) -> Task<Message> {
        let Some(list) = debug_env("FBTERM_OPEN_TABS") else {
            return Task::none();
        };
        let list = list.to_string_lossy().into_owned();
        let Some(&wid) = self.windows.keys().next() else {
            return Task::none();
        };
        let ids: Vec<ProfileId> = list
            .split(',')
            .filter_map(|n| self.profiles.profiles.iter().find(|p| p.name == n.trim()))
            .map(|p| p.id)
            .collect();
        let tasks: Vec<_> = ids.into_iter().map(|id| self.open_tab(wid, id)).collect();
        // They open in the background; the first tab stays active.
        if let Some(w) = self.windows.get_mut(&wid) {
            w.active = 0;
        }
        Task::batch(tasks)
    }

    /// `FBTERM_PALETTE_QUERY=text` (debug aid): the palette open and pre-filled in the first window.
    pub(crate) fn open_debug_palette(&mut self) -> Task<Message> {
        let Some(query) = debug_env("FBTERM_PALETTE_QUERY") else {
            return Task::none();
        };
        match self.windows.keys().next().copied() {
            Some(wid) => self.open_palette(wid, query.to_string_lossy().into_owned()),
            None => Task::none(),
        }
    }

    /// `FBTERM_OPEN_EDITOR=<profile name>` (debug aid): open the editor at startup with that
    /// profile selected (any other value selects the first).
    pub(crate) fn open_debug_editor(&mut self) -> Task<Message> {
        let Some(name) = debug_env("FBTERM_OPEN_EDITOR") else {
            return Task::none();
        };
        let task = self.open_editor();
        let name = name.to_string_lossy().into_owned();
        if let Some(p) = self.profiles.profiles.iter().find(|p| p.name == name)
            && let Some((_, ed)) = &mut self.editor
        {
            ed.update(EditorMsg::Select(p.id));
        }
        task
    }

    /// `FBTERM_DEMO_PROMPT=hostkey|password|changed` (debug aid): show a sample dialog in the
    /// first window. Nothing waits for its answer.
    pub(crate) fn open_debug_prompt(&mut self) -> Task<Message> {
        let Some(kind) = debug_env("FBTERM_DEMO_PROMPT") else {
            return Task::none();
        };
        let Some(&w) = self.windows.keys().next() else {
            return Task::none();
        };
        let dialog = match kind.to_string_lossy().as_ref() {
            "hostkey" => Ask::HostKey {
                host: "127.0.0.1".into(),
                port: 2222,
                fingerprint: "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s".into(),
                reply: tokio::sync::oneshot::channel().0,
            },
            "password" => Ask::Secret {
                profile: "bastion".into(),
                kind: SecretKind::Password,
                retry: true,
                reply: tokio::sync::oneshot::channel().0,
            },
            "changed" => {
                self.prompts.push(
                    w,
                    None,
                    Dialog::Changed {
                        host: "127.0.0.1".into(),
                        port: 2222,
                    },
                );
                return Task::none();
            }
            other => {
                tracing::warn!(%other, "unknown FBTERM_DEMO_PROMPT");
                return Task::none();
            }
        };
        self.prompts.ask(w, u64::MAX, dialog);
        self.focus_prompt(w)
    }
}
