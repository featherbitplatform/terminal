//! Quick-launch palette: fuzzy search over profiles and actions, per window.

use std::collections::{BTreeMap, HashMap};

use fbterm_config::{ProfileId, ProfileKind, ProfileStore};
use iced::widget::{
    button, column, container, mouse_area, opaque, row, scrollable, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Shadow, Vector};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::app::Message;
use crate::keys::{Action, Bindings};
use crate::theme::{Tokens, UI_FONT};

/// Rows shown at once.
const VISIBLE_ROWS: usize = 10;
const ROW_HEIGHT: f32 = 36.0;
const CARD_WIDTH: f32 = 560.0;

/// What the palette is asked to do. `Pick` is a click on a row (by position in the ranked list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteMsg {
    Open,
    Query(String),
    Up,
    Down,
    Submit,
    Pick(usize),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteItem {
    Profile {
        id: ProfileId,
        name: String,
        /// `user@host` (plus ` via <hops>`) for SSH, the program path for local shells.
        detail: String,
    },
    Action {
        action: Action,
        label: String,
    },
}

impl PaletteItem {
    fn label(&self) -> &str {
        match self {
            Self::Profile { name, .. } => name,
            Self::Action { label, .. } => label,
        }
    }

    fn is_action(&self) -> bool {
        matches!(self, Self::Action { .. })
    }
}

/// When each profile was last opened (unix seconds). Persisted by the state file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recency(BTreeMap<ProfileId, i64>);

#[allow(dead_code, reason = "`From` conversions are used by state persistence")]
impl Recency {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bump(&mut self, id: ProfileId, now: i64) {
        self.0.insert(id, now);
    }

    pub fn get(&self, id: ProfileId) -> Option<i64> {
        self.0.get(&id).copied()
    }
}

impl From<BTreeMap<ProfileId, i64>> for Recency {
    fn from(map: BTreeMap<ProfileId, i64>) -> Self {
        Self(map)
    }
}

impl From<Recency> for BTreeMap<ProfileId, i64> {
    fn from(recency: Recency) -> Self {
        recency.0
    }
}

/// The M1 actions, in the order they are listed.
fn actions() -> Vec<PaletteItem> {
    [
        (Action::NewTab, "New tab"),
        (Action::NewWindow, "New window"),
        (Action::CloseTab, "Close pane"),
        (Action::SplitRight, "Split right"),
        (Action::SplitDown, "Split down"),
        (Action::MaximizePane, "Maximize pane"),
        (Action::Find, "Find in tab"),
        (Action::FindAll, "Find in all tabs"),
        (Action::ExportTab, "Export tab to file…"),
        (Action::EditProfiles, "Edit profiles"),
        (Action::Settings, "Settings"),
        (Action::OpenSettingsFile, "Open settings file"),
        (Action::RevealConfigFolder, "Reveal config folder"),
        (Action::Quit, "Quit"),
    ]
    .into_iter()
    .map(|(action, label)| PaletteItem::Action {
        action,
        label: label.into(),
    })
    .collect()
}

/// The chord shown next to a palette action: its current one, if it has one.
pub fn action_hint(action: Action, bindings: &Bindings) -> Option<String> {
    bindings.label(action)
}

/// Every profile of the store, then the actions.
pub fn items(store: &ProfileStore) -> Vec<PaletteItem> {
    let mut out: Vec<PaletteItem> = store
        .profiles
        .iter()
        .map(|p| {
            let detail = match &p.kind {
                ProfileKind::Local(l) => l.program.display().to_string(),
                ProfileKind::Ssh(s) => {
                    let mut d = format!("{}@{}", s.user, s.host);
                    if let Ok(hops) = store.jump_chain(p.id)
                        && !hops.is_empty()
                    {
                        let names: Vec<&str> = hops.iter().map(|h| h.name.as_str()).collect();
                        d.push_str(" via ");
                        d.push_str(&names.join(" > "));
                    }
                    d
                }
            };
            PaletteItem::Profile {
                id: p.id,
                name: p.name.clone(),
                detail,
            }
        })
        .collect();
    out.extend(actions());
    out
}

/// Indices into `items`, best first. Empty query: recent profiles (latest first), the other
/// profiles alphabetically, then the actions in their listed order. Otherwise only matches:
/// profiles before actions, each by score, then recency, then name.
pub fn rank(query: &str, items: &[PaletteItem], recency: &Recency) -> Vec<usize> {
    let recent = |item: &PaletteItem| match item {
        PaletteItem::Profile { id, .. } => recency.get(*id),
        PaletteItem::Action { .. } => None,
    };
    let name_key = |i: usize| items[i].label().to_lowercase();

    if query.trim().is_empty() {
        let mut order: Vec<usize> = (0..items.len()).collect();
        order.sort_by(|&a, &b| {
            let (ia, ib) = (&items[a], &items[b]);
            ia.is_action().cmp(&ib.is_action()).then_with(|| {
                if ia.is_action() {
                    a.cmp(&b)
                } else {
                    // Some(_) sorts after None, so reverse: recent first, never-opened last.
                    recent(ib)
                        .cmp(&recent(ia))
                        .then_with(|| name_key(a).cmp(&name_key(b)))
                }
            })
        });
        return order;
    }

    // Every whitespace-separated word is a literal fuzzy atom (no `!`, `^`, `'` or `$` syntax),
    // matched against name and detail together so one query can span both.
    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut score = |s: &str| pattern.score(Utf32Str::new(s, &mut buf), &mut matcher);

    let mut scored: Vec<(usize, u32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let best = match item {
                PaletteItem::Profile { name, detail, .. } => score(&format!("{name} {detail}")),
                PaletteItem::Action { label, .. } => score(label),
            };
            best.map(|s| (i, s))
        })
        .collect();
    scored.sort_by(|&(a, sa), &(b, sb)| {
        items[a]
            .is_action()
            .cmp(&items[b].is_action())
            .then(sb.cmp(&sa))
            .then_with(|| recent(&items[b]).cmp(&recent(&items[a])))
            .then_with(|| name_key(a).cmp(&name_key(b)))
            .then(a.cmp(&b))
    });
    scored.into_iter().map(|(i, _)| i).collect()
}

/// One window's open palette.
#[derive(Debug)]
pub struct PaletteState {
    pub query: String,
    items: Vec<PaletteItem>,
    ranked: Vec<usize>,
    selected: usize,
    pub input: iced::widget::Id,
}

impl PaletteState {
    pub fn new(items: Vec<PaletteItem>, query: String, recency: &Recency) -> Self {
        let ranked = rank(&query, &items, recency);
        Self {
            query,
            items,
            ranked,
            selected: 0,
            input: iced::widget::Id::unique(),
        }
    }

    /// Replace the listed items (the profiles changed), keeping the query and the input.
    pub fn set_items(&mut self, items: Vec<PaletteItem>, recency: &Recency) {
        self.items = items;
        self.ranked = rank(&self.query, &self.items, recency);
        self.selected = self.selected.min(self.ranked.len().saturating_sub(1));
    }

    pub fn set_query(&mut self, query: String, recency: &Recency) {
        self.ranked = rank(&query, &self.items, recency);
        self.query = query;
        self.selected = 0;
    }

    /// Selection moves with wrap-around.
    pub fn up(&mut self) {
        let n = self.ranked.len();
        if n > 0 {
            self.selected = (self.selected + n - 1) % n;
        }
    }

    pub fn down(&mut self) {
        let n = self.ranked.len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    /// The item at `position` in the ranked list.
    pub fn at(&self, position: usize) -> Option<&PaletteItem> {
        self.ranked.get(position).map(|&i| &self.items[i])
    }

    pub fn selected_item(&self) -> Option<&PaletteItem> {
        self.at(self.selected)
    }

    #[cfg(test)]
    pub fn selected_position(&self) -> usize {
        self.selected
    }

    #[cfg(test)]
    pub fn ranked_len(&self) -> usize {
        self.ranked.len()
    }
}

/// Palettes of the windows that have one open.
pub type Palettes = HashMap<iced::window::Id, PaletteState>;

/// The palette overlay: a dimmed backdrop (click to close) with the card near the top.
pub fn view<'a>(
    t: &'static Tokens,
    window: iced::window::Id,
    state: &'a PaletteState,
    bindings: &Bindings,
) -> Element<'a, Message> {
    let msg = move |m| Message::Palette(window, m);
    let input = text_input("Search profiles and actions", &state.query)
        .id(state.input.clone())
        .on_input(move |q| msg(PaletteMsg::Query(q)))
        .font(UI_FONT)
        .size(15)
        .padding(Padding::from([14.0, 16.0]))
        .style(move |_, _| text_input::Style {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            icon: t.text_muted,
            placeholder: t.text_muted,
            value: t.text,
            selection: t.accent_soft,
        });

    let offset = (state.selected + 1).saturating_sub(VISIBLE_ROWS);
    let mut rows = column![];
    for (pos, &idx) in state
        .ranked
        .iter()
        .enumerate()
        .skip(offset)
        .take(VISIBLE_ROWS)
    {
        rows = rows.push(item_row(
            t,
            window,
            pos,
            &state.items[idx],
            pos == state.selected,
            bindings,
        ));
    }
    let list: Element<'a, Message> = if state.ranked.is_empty() {
        container(
            text("No matches")
                .font(UI_FONT)
                .size(13)
                .color(t.text_muted),
        )
        .padding(Padding::from([14.0, 16.0]))
        .into()
    } else {
        scrollable(rows)
            .height(Length::Shrink)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .into()
    };

    let divider = container(text(""))
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    let card = container(column![input, divider, list])
        .width(CARD_WIDTH)
        .max_height(ROW_HEIGHT * VISIBLE_ROWS as f32 + 60.0)
        .clip(true)
        .style(move |_| container::Style {
            background: Some(t.raised.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 12.0.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
                offset: Vector::new(0.0, 12.0),
                blur_radius: 32.0,
            },
            ..container::Style::default()
        });
    let backdrop = container(opaque(card))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .padding(Padding {
            top: 72.0,
            ..Padding::ZERO
        })
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.32).into()),
            ..container::Style::default()
        });
    mouse_area(backdrop).on_press(msg(PaletteMsg::Close)).into()
}

fn item_row<'a>(
    t: &'static Tokens,
    window: iced::window::Id,
    position: usize,
    item: &'a PaletteItem,
    selected: bool,
    bindings: &Bindings,
) -> Element<'a, Message> {
    let mono = iced::Font::with_name("Geist Mono");
    let edge = container(text(""))
        .width(2)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: selected.then(|| t.accent_edge.into()),
            ..container::Style::default()
        });
    let (label, detail) = match item {
        PaletteItem::Profile { name, detail, .. } => (name.as_str(), Some(detail.as_str())),
        PaletteItem::Action { label, .. } => (label.as_str(), None),
    };
    let mut content = row![
        text(label)
            .font(UI_FONT)
            .size(13.5)
            .wrapping(text::Wrapping::None)
            .color(t.text)
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    match detail {
        Some(d) => {
            content = content.push(
                container(
                    text(d)
                        .font(mono)
                        .size(12)
                        .wrapping(text::Wrapping::None)
                        .color(t.text_muted),
                )
                .width(Length::Fill)
                .align_x(Alignment::End)
                .clip(true),
            );
        }
        None => {
            // An action shows its current chord, or just that it is an action.
            let hint = match item {
                PaletteItem::Action { action, .. } => action_hint(*action, bindings),
                PaletteItem::Profile { .. } => None,
            };
            let hint: Element<'a, Message> = match hint {
                Some(chord) => text(chord)
                    .font(mono)
                    .size(12)
                    .color(t.text_secondary)
                    .into(),
                None => text("Action")
                    .font(UI_FONT)
                    .size(11.5)
                    .color(t.text_muted)
                    .into(),
            };
            content = content.push(container(hint).width(Length::Fill).align_x(Alignment::End));
        }
    }
    let inner = row![edge, container(content).padding(Padding::from([0.0, 14.0]))]
        .align_y(Alignment::Center)
        .height(Length::Fill);
    button(inner)
        .on_press(Message::Palette(window, PaletteMsg::Pick(position)))
        .width(Length::Fill)
        .height(ROW_HEIGHT)
        .padding(0)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if selected {
                    Some(t.accent_soft.into())
                } else {
                    hovered.then(|| t.hover.into())
                },
                text_color: t.text,
                border: Border::default(),
                ..button::Style::default()
            }
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, detail: &str) -> PaletteItem {
        PaletteItem::Profile {
            id: ProfileId::new(),
            name: name.into(),
            detail: detail.into(),
        }
    }

    fn action(action: Action, label: &str) -> PaletteItem {
        PaletteItem::Action {
            action,
            label: label.into(),
        }
    }

    fn id(item: &PaletteItem) -> ProfileId {
        match item {
            PaletteItem::Profile { id, .. } => *id,
            PaletteItem::Action { .. } => panic!("not a profile"),
        }
    }

    fn names<'a>(items: &'a [PaletteItem], order: &[usize]) -> Vec<&'a str> {
        order.iter().map(|&i| items[i].label()).collect()
    }

    #[test]
    fn empty_query_lists_recent_profiles_first_then_alphabetical_then_actions() {
        let items = vec![
            profile("zeta", "z@z"),
            action(Action::NewTab, "New tab"),
            profile("Beta", "b@b"),
            profile("alpha", "a@a"),
            profile("mid", "m@m"),
            action(Action::Settings, "Settings"),
        ];
        let mut recency = Recency::new();
        recency.bump(id(&items[0]), 100);
        recency.bump(id(&items[4]), 200);
        let order = rank("", &items, &recency);
        assert_eq!(
            names(&items, &order),
            ["mid", "zeta", "alpha", "Beta", "New tab", "Settings"]
        );
    }

    #[test]
    fn actions_list_find_settings_and_the_settings_file_actions() {
        let labels: Vec<String> = actions().iter().map(|a| a.label().to_string()).collect();
        assert_eq!(
            labels,
            [
                "New tab",
                "New window",
                "Close pane",
                "Split right",
                "Split down",
                "Maximize pane",
                "Find in tab",
                "Find in all tabs",
                "Export tab to file\u{2026}",
                "Edit profiles",
                "Settings",
                "Open settings file",
                "Reveal config folder",
                "Quit"
            ]
        );
    }

    #[test]
    fn fuzzy_matches_noncontiguous() {
        let items = vec![profile("prod-db-01", "u@h"), profile("staging", "u@h")];
        let order = rank("prdb", &items, &Recency::new());
        assert_eq!(names(&items, &order), ["prod-db-01"]);
    }

    #[test]
    fn profiles_rank_above_actions() {
        let items = vec![
            action(Action::NewWindow, "Split right"),
            profile("split-brain", "u@h"),
        ];
        let order = rank("split", &items, &Recency::new());
        assert_eq!(names(&items, &order), ["split-brain", "Split right"]);
    }

    #[test]
    fn tie_broken_by_recency() {
        let items = vec![profile("alpha-1", "x"), profile("alpha-2", "x")];
        let mut recency = Recency::new();
        recency.bump(id(&items[1]), 50);
        let order = rank("alpha", &items, &recency);
        assert_eq!(names(&items, &order), ["alpha-2", "alpha-1"]);
        recency.bump(id(&items[0]), 90);
        let order = rank("alpha", &items, &recency);
        assert_eq!(names(&items, &order), ["alpha-1", "alpha-2"]);
    }

    #[test]
    fn no_match_returns_empty() {
        let items = vec![profile("prod", "u@h"), action(Action::Settings, "Settings")];
        assert!(rank("qqqq", &items, &Recency::new()).is_empty());
    }

    #[test]
    fn matches_detail_too() {
        let items = vec![
            profile("db", "root@10.0.4.17"),
            profile("web", "root@192.168.1.5"),
        ];
        let order = rank("10.0.4", &items, &Recency::new());
        assert_eq!(names(&items, &order), ["db"]);
    }

    #[test]
    fn query_syntax_characters_match_literally() {
        let items = vec![
            profile("!bang", "u@h"),
            profile("^caret", "u@h"),
            profile("it's", "u@h"),
            profile("cost$", "u@h"),
            profile("plain", "u@h"),
        ];
        let r = |q: &str| names(&items, &rank(q, &items, &Recency::new())).join(",");
        assert_eq!(r("!bang"), "!bang");
        assert_eq!(r("^car"), "^caret");
        assert_eq!(r("'s"), "it's");
        assert_eq!(r("t$"), "cost$");
        assert_eq!(r("!plain"), "", "! is not negation");
    }

    #[test]
    fn multi_word_query_spans_name_and_detail() {
        let items = vec![
            profile("prod", "root@10.0.4.17"),
            profile("prod-web", "deploy@10.0.4.18"),
            profile("staging", "root@10.0.9.1"),
        ];
        let order = rank("prod root", &items, &Recency::new());
        assert_eq!(names(&items, &order), ["prod"]);
    }

    #[test]
    fn selection_wraps_and_resets_on_query() {
        let items = vec![profile("a", ""), profile("b", ""), profile("c", "")];
        let recency = Recency::new();
        let mut s = PaletteState::new(items, String::new(), &recency);
        s.up();
        assert_eq!(s.selected_position(), s.ranked_len() - 1);
        s.down();
        assert_eq!(s.selected_position(), 0);
        s.down();
        s.set_query("a".into(), &recency);
        assert_eq!(s.selected_position(), 0);
        assert!(matches!(
            s.selected_item(),
            Some(PaletteItem::Profile { name, .. }) if name == "a"
        ));
    }

    #[test]
    fn recency_converts_to_and_from_a_map() {
        let a = ProfileId::new();
        let mut r = Recency::new();
        r.bump(a, 7);
        let map: BTreeMap<ProfileId, i64> = r.clone().into();
        assert_eq!(map.get(&a), Some(&7));
        assert_eq!(Recency::from(map), r);
        assert_eq!(r.get(ProfileId::new()), None);
    }

    #[test]
    fn items_describe_ssh_profiles_with_their_jump_chain() {
        use fbterm_config::{AuthMethod, Profile, SshProfile};
        let ssh = |name: &str, host: &str, jump: Vec<ProfileId>| Profile {
            id: ProfileId::new(),
            name: name.into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            kind: ProfileKind::Ssh(SshProfile {
                host: host.into(),
                port: 22,
                user: "root".into(),
                auth: AuthMethod::Password,
                jump,
            }),
        };
        let bastion = ssh("bastion", "b.example.com", vec![]);
        let web = ssh("web", "10.0.0.5", vec![bastion.id]);
        let store = ProfileStore {
            profiles: vec![bastion, web],
        };
        let list = items(&store);
        let detail = |n: &str| {
            list.iter().find_map(|i| match i {
                PaletteItem::Profile { name, detail, .. } if name == n => Some(detail.clone()),
                _ => None,
            })
        };
        assert_eq!(detail("bastion").as_deref(), Some("root@b.example.com"));
        assert_eq!(detail("web").as_deref(), Some("root@10.0.0.5 via bastion"));
        assert_eq!(list.iter().filter(|i| i.is_action()).count(), 14);
    }

    #[test]
    fn actions_show_their_current_chord() {
        let b = Bindings::defaults(true);
        assert_eq!(action_hint(Action::NewTab, &b).as_deref(), Some("⌘T"));
        assert_eq!(action_hint(Action::OpenSettingsFile, &b), None);
        let mut o = fbterm_config::keys::Overrides::new();
        o.insert(
            Action::OpenSettingsFile,
            fbterm_config::keys::Chord::parse("cmd+alt+o").unwrap(),
        );
        o.insert(Action::NewTab, None);
        let b = Bindings::new(true, &o);
        assert_eq!(
            action_hint(Action::OpenSettingsFile, &b).as_deref(),
            Some("⌥⌘O")
        );
        assert_eq!(action_hint(Action::NewTab, &b), None);
    }
}
