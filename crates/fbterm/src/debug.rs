//! Debug-build aids that set up a scene at startup (see the list in `main.rs`).

use fbterm_config::ProfileId;
use fbterm_secrets::SecretKind;
use iced::Task;

use crate::app::{App, Message, debug_env};
use crate::editor::EditorMsg;
use crate::layout::{Axis, Layout};
use crate::prompts::{Ask, Dialog};
use crate::tabs::Pane;

/// Each join: the axis and the index of the name joined to everything before it.
type Joins = Vec<(Axis, usize)>;

/// Parses an `FBTERM_SPLITS` layout: profile names joined by `|` (side by side) and `/`
/// (stacked), binding left to right with equal halves: `A|B/C` is `A` beside `B`, both above
/// `C`. Returns the names in reading order and a tree over their indices.
fn parse_splits(spec: &str) -> Option<(Vec<String>, Joins)> {
    let mut names = Vec::new();
    let mut joins = Vec::new();
    let mut axis = None;
    let mut name = String::new();
    for c in spec.chars().chain(std::iter::once('\0')) {
        let next = match c {
            '|' => Some(Axis::Row),
            '/' => Some(Axis::Column),
            '\0' => None,
            _ => {
                name.push(c);
                continue;
            }
        };
        let n = name.trim().to_string();
        if n.is_empty() {
            return None;
        }
        names.push(n);
        name.clear();
        if let Some(a) = axis {
            joins.push((a, names.len() - 1));
        }
        axis = next;
    }
    Some((names, joins))
}

impl App {
    /// `FBTERM_OPEN_TABS=name,name` (debug aid): extra tabs opened in the first window at
    /// startup, by profile name. Such runs do not save the session (`state_saving_enabled`).
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

    /// `FBTERM_SPLITS=<layout>` (debug aid): the first tab of the first window replaced by
    /// panes of these profiles (by name) in this layout (see `parse_splits`), for screenshots.
    pub(crate) fn open_debug_splits(&mut self) -> Task<Message> {
        let Some(spec) = debug_env("FBTERM_SPLITS") else {
            return Task::none();
        };
        let Some((names, joins)) = parse_splits(&spec.to_string_lossy()) else {
            tracing::warn!("FBTERM_SPLITS: not a layout");
            return Task::none();
        };
        let Some(panes) = names
            .iter()
            .map(|n| self.profiles.profiles.iter().find(|p| p.name == *n))
            .map(|p| p.map(Pane::for_profile))
            .collect::<Option<Vec<Pane>>>()
        else {
            tracing::warn!("FBTERM_SPLITS: unknown profile name");
            return Task::none();
        };
        let Some(&wid) = self.windows.keys().next() else {
            return Task::none();
        };
        let ids: Vec<_> = panes.iter().map(|p| p.id).collect();
        let mut layout = Layout::Pane(ids[0]);
        for (axis, i) in joins {
            layout = Layout::Split {
                axis,
                ratio: 0.5,
                first: Box::new(layout),
                second: Box::new(Layout::Pane(ids[i])),
            };
        }
        let Some(tab) = self.windows.get_mut(&wid).and_then(|w| w.tabs.first_mut()) else {
            return Task::none();
        };
        tab.layout = layout;
        tab.panes = panes;
        tab.focused = ids[0];
        tab.maximized = None;
        tab.recent = vec![ids[0]];
        let tasks: Vec<_> = ids.into_iter().map(|id| self.start(wid, id)).collect();
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

    /// `FBTERM_OPEN_SETTINGS=1` (debug aid): open the Settings window at startup.
    pub(crate) fn open_debug_settings(&mut self) -> Task<Message> {
        if debug_env("FBTERM_OPEN_SETTINGS").is_none() {
            return Task::none();
        }
        self.open_settings()
    }

    /// `FBTERM_FIND=<text>` / `FBTERM_FIND_ALL=<text>` (debug aids): the find bar of the first
    /// window's active tab, or the find-in-all-tabs box, open with that query once the tabs
    /// have had a second to print something.
    pub(crate) fn open_debug_find(&mut self) -> Task<Message> {
        let tasks = [("FBTERM_FIND", false), ("FBTERM_FIND_ALL", true)]
            .into_iter()
            .filter_map(|(name, all)| {
                let query = debug_env(name)?.to_string_lossy().into_owned();
                Some(Task::perform(
                    async { tokio::time::sleep(std::time::Duration::from_millis(900)).await },
                    move |()| Message::DebugFind {
                        all,
                        query: query.clone(),
                    },
                ))
            });
        Task::batch(tasks)
    }

    pub(crate) fn debug_find(&mut self, all: bool, query: String) -> Task<Message> {
        let Some(&wid) = self.windows.keys().next() else {
            return Task::none();
        };
        if all {
            return self.open_find_all(wid, query);
        }
        // Open, then type: the field ends with the cursor after the query.
        let _ = self.find(wid, crate::find::FindMsg::Open);
        let search = self.find(wid, crate::find::FindMsg::Query(query));
        let input = self
            .windows
            .get(&wid)
            .and_then(|w| w.active_tab())
            .and_then(|t| t.focused_pane().find.as_ref())
            .map(|bar| bar.input.clone());
        let focus = input.map_or(Task::none(), |id| {
            iced::widget::operation::focus(id.clone())
                .chain(iced::widget::operation::move_cursor_to_end(id))
        });
        Task::batch([search, focus])
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

#[cfg(test)]
mod tests {
    use super::parse_splits;
    use crate::layout::Axis;

    #[test]
    fn splits_bind_left_to_right() {
        let (names, joins) = parse_splits("A|B / C").unwrap();
        assert_eq!(names, ["A", "B", "C"]);
        assert_eq!(joins, [(Axis::Row, 1), (Axis::Column, 2)]);
        assert_eq!(parse_splits("solo").unwrap().1, []);
        assert!(parse_splits("A||B").is_none());
        assert!(parse_splits("A|").is_none());
    }
}
