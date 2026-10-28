//! Color schemes applied to panes, against a temporary config directory.

use fbterm_config::schemes::{Scheme, SchemeColors, new_user_id, save_user};
use fbterm_config::{
    AuthMethod, Paths, Profile, ProfileId, ProfileKind, ProfileStore, Rgb, SshProfile,
};
use fbterm_term::{ColorScheme, SharedTerminal, TerminalStyle};
use iced::{Font, window};

use crate::app::{App, DEFAULT_GRID, Message};
use crate::layout::{PaneId, Side};
use crate::restore_tests::{paths_with, profile};
use crate::tabs::{Pane, Tab, WindowState};
use crate::watch::Changed;

const SCHEMES_CHANGED: Changed = Changed {
    profiles: false,
    settings: false,
    schemes: true,
};
const SETTINGS_CHANGED: Changed = Changed {
    profiles: false,
    settings: true,
    schemes: false,
};
const PROFILES_CHANGED: Changed = Changed {
    profiles: true,
    settings: false,
    schemes: false,
};

pub(crate) fn rgb(v: u32) -> Rgb {
    Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    }
}

/// A user scheme: Graphite's colors on background `bg`.
pub(crate) fn user_scheme(name: &str, bg: u32) -> Scheme {
    let graphite = fbterm_config::schemes::builtins().remove(0);
    Scheme {
        id: new_user_id(),
        name: name.into(),
        colors: SchemeColors {
            background: rgb(bg),
            ..graphite.colors
        },
        font_family: None,
        font_size: None,
        credit: None,
        gallery: None,
        builtin: false,
    }
}

pub(crate) fn with_scheme(p: &Profile, id: &str) -> Profile {
    Profile {
        color_scheme: Some(id.into()),
        ..p.clone()
    }
}

/// An app over `profiles`, `settings.toml` text and user `schemes`, with one window whose
/// only tab shows one pane per profile, each with a placeholder terminal (no session).
pub(crate) fn app_with(
    profiles: &[Profile],
    settings: &str,
    schemes: &[Scheme],
) -> (App, Paths, window::Id, Vec<PaneId>) {
    let paths = paths_with(profiles);
    std::fs::write(paths.settings(), settings).unwrap();
    for s in schemes {
        save_user(&paths.schemes(), s).unwrap();
    }
    let (mut app, _) = App::build(paths.clone(), true);
    app.state_saving_enabled = false;
    let mut panes: Vec<Pane> = profiles.iter().map(Pane::for_profile).collect();
    for pane in &mut panes {
        let term = app.new_terminal(DEFAULT_GRID);
        app.scheme_colors(pane).apply_to(&mut term.lock());
        pane.placeholder = Some(term);
    }
    let ids: Vec<PaneId> = panes.iter().map(|p| p.id).collect();
    let mut panes = panes.into_iter();
    let mut tab = Tab::single(panes.next().unwrap());
    for pane in panes {
        let last = *tab.panes.last().map(|p| &p.id).unwrap();
        tab.layout.split(last, Side::Right, pane.id);
        tab.panes.push(pane);
    }
    let wid = window::Id::unique();
    let mut w = WindowState::new();
    w.push(tab);
    app.windows.clear();
    app.windows.insert(wid, w);
    (app, paths, wid, ids)
}

pub(crate) fn pane(app: &App, wid: window::Id, id: PaneId) -> &Pane {
    app.windows[&wid].tabs[0].pane(id).unwrap()
}

pub(crate) fn style(app: &App, wid: window::Id, id: PaneId) -> TerminalStyle {
    let tab = &app.windows[&wid].tabs[0];
    app.pane_style(wid, tab, tab.pane(id).unwrap())
}

pub(crate) fn term(app: &App, wid: window::Id, id: PaneId) -> SharedTerminal {
    pane(app, wid, id).terminal().unwrap().clone()
}

/// What the terminal answers a program asking for its background (OSC 11).
fn background_reply(term: &SharedTerminal) -> String {
    let mut t = term.lock();
    let _ = t.take_events();
    t.feed(b"\x1b]11;?\x07");
    let bytes: Vec<u8> = t
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            fbterm_term::TermEvent::PtyWrite(b) => Some(b),
            _ => None,
        })
        .flatten()
        .collect();
    String::from_utf8(bytes).unwrap()
}

pub(crate) fn answers(term: &SharedTerminal, bg: Rgb) -> bool {
    let Rgb { r, g, b } = bg;
    background_reply(term).contains(&format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}"))
}

pub(crate) fn builtin(id: &str) -> Scheme {
    fbterm_config::schemes::builtins()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

pub(crate) fn generation(app: &App, wid: window::Id, id: PaneId) -> u64 {
    term(app, wid, id).lock().generation()
}

#[test]
fn default_panes_use_graphite_unchanged() {
    let a = profile("a");
    let (app, _, wid, ids) = app_with(std::slice::from_ref(&a), "", &[]);
    assert_eq!(
        ColorScheme::from(&builtin("graphite").colors),
        ColorScheme::graphite()
    );
    // Today's look: Graphite, the settings' font and size, the background left to the window.
    let s = style(&app, wid, ids[0]);
    let today = TerminalStyle {
        scheme: ColorScheme::graphite(),
        font: Font::with_name("Geist Mono"),
        font_size: app.settings.font_size,
        accent: None,
        focused: s.focused,
        background_opacity: 0.0,
    };
    assert_eq!(s, today);
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "graphite");
    assert!(answers(&term(&app, wid, ids[0]), rgb(0x0b0b0e)));
    assert!(app.notices.is_empty(), "{:?}", app.notices);
}

#[test]
fn global_setting_changes_all_unoverridden_panes() {
    let (a, b, c) = (profile("a"), profile("b"), profile("c"));
    let a = with_scheme(&a, "harbor");
    let (mut app, paths, wid, ids) = app_with(&[a, b, c], "", &[]);
    let untouched = generation(&app, wid, ids[0]);

    std::fs::write(paths.settings(), "color_scheme = \"ember\"\n").unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));

    let ember = builtin("ember");
    let harbor = builtin("harbor");
    for &id in &ids[1..] {
        assert_eq!(app.scheme_for(pane(&app, wid, id)).id, "ember");
        assert_eq!(
            style(&app, wid, id).scheme,
            ColorScheme::from(&ember.colors)
        );
        assert!(answers(&term(&app, wid, id), ember.colors.background));
    }
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "harbor");
    assert_eq!(
        generation(&app, wid, ids[0]),
        untouched,
        "override untouched"
    );
    assert!(answers(&term(&app, wid, ids[0]), harbor.colors.background));
}

#[test]
fn profile_override_wins() {
    let a = with_scheme(&profile("a"), "harbor");
    let b = profile("b");
    let (mut app, paths, wid, ids) =
        app_with(&[a.clone(), b.clone()], "color_scheme = \"ember\"\n", &[]);
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "harbor");
    assert_eq!(
        style(&app, wid, ids[0]).scheme,
        ColorScheme::from(&builtin("harbor").colors)
    );
    assert_eq!(app.scheme_for(pane(&app, wid, ids[1])).id, "ember");

    // Saving the profile with another scheme re-colors its panes only.
    let untouched = generation(&app, wid, ids[1]);
    ProfileStore {
        profiles: vec![with_scheme(&a, "graphite-light"), b],
    }
    .save(&paths.profiles())
    .unwrap();
    let _ = app.update(Message::ConfigChanged(PROFILES_CHANGED));
    assert_eq!(
        pane(&app, wid, ids[0]).scheme.as_deref(),
        Some("graphite-light")
    );
    let light = builtin("graphite-light");
    assert!(answers(&term(&app, wid, ids[0]), light.colors.background));
    assert_eq!(generation(&app, wid, ids[1]), untouched);
}

#[test]
fn scheme_change_touches_only_its_panes() {
    let mine = user_scheme("Mine", 0x101820);
    let a = with_scheme(&profile("a"), "harbor");
    let b = with_scheme(&profile("b"), &mine.id);
    let c = profile("c");
    let (mut app, paths, wid, ids) = app_with(&[a, b, c], "", std::slice::from_ref(&mine));
    assert!(answers(&term(&app, wid, ids[1]), rgb(0x101820)));
    let (ga, gc) = (generation(&app, wid, ids[0]), generation(&app, wid, ids[2]));

    let changed = Scheme {
        colors: SchemeColors {
            background: rgb(0x202830),
            ..mine.colors
        },
        ..mine.clone()
    };
    save_user(&paths.schemes(), &changed).unwrap();
    let _ = app.update(Message::ConfigChanged(SCHEMES_CHANGED));

    assert!(answers(&term(&app, wid, ids[1]), rgb(0x202830)));
    assert_eq!(style(&app, wid, ids[1]).scheme.background, rgb(0x202830));
    assert_eq!(generation(&app, wid, ids[0]), ga);
    assert_eq!(generation(&app, wid, ids[2]), gc);
}

#[test]
fn missing_profile_scheme_falls_back_with_notice() {
    let a = with_scheme(&profile("Sample"), "user-missing");
    let (app, _, wid, ids) = app_with(std::slice::from_ref(&a), "color_scheme = \"harbor\"\n", &[]);
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "harbor");
    let line =
        "Color scheme \"user-missing\" not found for profile \"Sample\"; using the global scheme.";
    assert_eq!(
        app.notices.iter().filter(|n| *n == line).count(),
        1,
        "{:?}",
        app.notices
    );
    assert!(app.visible_notices().iter().any(|n| n == line));
}

#[test]
fn missing_global_falls_back_to_graphite() {
    let a = profile("a");
    let (mut app, paths, wid, ids) = app_with(
        std::slice::from_ref(&a),
        "color_scheme = \"nowhere\"\n",
        &[],
    );
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "graphite");
    let line = "Color scheme \"nowhere\" not found; using Graphite.";
    assert_eq!(app.notices, vec![line.to_owned()]);
    // Reloading the same settings says it once, not twice.
    std::fs::write(paths.settings(), "color_scheme = \"nowhere\"\n# touched\n").unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert_eq!(app.notices, vec![line.to_owned()]);
    // Fixed: the line goes.
    std::fs::write(paths.settings(), "color_scheme = \"ember\"\n").unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert!(app.notices.is_empty(), "{:?}", app.notices);
}

#[test]
fn scheme_font_sets_pane_family_and_size() {
    let mut mine = user_scheme("Mine", 0x101820);
    mine.font_family = Some("Sample Mono".into());
    mine.font_size = Some(20.0);
    let a = with_scheme(&profile("a"), &mine.id);
    let b = profile("b");
    let (mut app, _, wid, ids) = app_with(&[a, b], "font_size = 13.0\n", &[mine]);
    let s = style(&app, wid, ids[0]);
    assert_eq!(s.font, Font::with_name("Sample Mono"));
    assert_eq!(s.font_size, 20.0);
    let other = style(&app, wid, ids[1]);
    assert_eq!(other.font_size, 13.0);
    assert_eq!(
        other.font,
        Font::with_name(&*app.settings.font_family.clone().leak())
    );

    // Zoom stays relative to the scheme's size, and clamps the same way.
    let tab = &mut app.windows.get_mut(&wid).unwrap().tabs[0];
    tab.panes.iter_mut().find(|p| p.id == ids[0]).unwrap().zoom = 2;
    assert_eq!(style(&app, wid, ids[0]).font_size, 22.0);
    let tab = &mut app.windows.get_mut(&wid).unwrap().tabs[0];
    tab.panes.iter_mut().find(|p| p.id == ids[0]).unwrap().zoom = 40;
    assert_eq!(style(&app, wid, ids[0]).font_size, 48.0);
}

#[test]
fn osc_query_answers_with_pane_scheme() {
    let a = with_scheme(&profile("a"), "harbor");
    let b = profile("b");
    let paths = paths_with(&[a.clone(), b.clone()]);
    let (mut app, _) = App::build(paths, true);
    app.state_saving_enabled = false;
    let (pa, pb) = (Pane::for_profile(&a), Pane::for_profile(&b));
    let (ia, ib) = (pa.id, pb.id);
    let mut tab = Tab::single(pa);
    tab.layout.split(ia, Side::Right, ib);
    tab.panes.push(pb);
    let wid = window::Id::unique();
    let mut w = WindowState::new();
    w.push(tab);
    app.windows.clear();
    app.windows.insert(wid, w);
    // Each start fails at once (no such program) and leaves the pane its terminal.
    let _ = app.start(wid, ia);
    let _ = app.start(wid, ib);
    assert!(answers(
        &term(&app, wid, ia),
        builtin("harbor").colors.background
    ));
    assert!(answers(&term(&app, wid, ib), rgb(0x0b0b0e)));
}

#[test]
fn new_ssh_or_restarted_terminal_gets_pane_scheme() {
    let ssh = Profile {
        id: ProfileId::new(),
        name: "remote".into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: Some("ember".into()),
        kind: ProfileKind::Ssh(SshProfile {
            host: "127.0.0.1".into(),
            port: 1,
            user: "sample".into(),
            auth: AuthMethod::Agent,
            jump: vec![],
            tunnels: Vec::new(),
        }),
    };
    let local = profile("local");
    let paths = paths_with(&[ssh.clone(), local.clone()]);
    let (mut app, _) = App::build(paths, true);
    app.state_saving_enabled = false;
    let (ps, pl) = (Pane::for_profile(&ssh), Pane::for_profile(&local));
    let (is, il) = (ps.id, pl.id);
    let mut tab = Tab::single(ps);
    tab.layout.split(is, Side::Right, il);
    tab.panes.push(pl);
    let wid = window::Id::unique();
    let mut w = WindowState::new();
    w.push(tab);
    app.windows.clear();
    app.windows.insert(wid, w);

    // The dial is never run here: the pane shows its terminal while connecting.
    drop(app.start(wid, is));
    assert!(answers(
        &term(&app, wid, is),
        builtin("ember").colors.background
    ));

    // A restart continues in the same terminal and takes the pane's scheme as it is now.
    let _ = app.start(wid, il);
    let first = term(&app, wid, il);
    assert!(answers(&first, rgb(0x0b0b0e)));
    let tab = &mut app.windows.get_mut(&wid).unwrap().tabs[0];
    tab.panes.iter_mut().find(|p| p.id == il).unwrap().scheme = Some("harbor".into());
    let _ = app.start(wid, il);
    let again = term(&app, wid, il);
    assert!(std::sync::Arc::ptr_eq(&first, &again), "same terminal");
    assert!(answers(&again, builtin("harbor").colors.background));
}

#[test]
fn schemes_dir_change_reloads() {
    let mine = user_scheme("Mine", 0x101820);
    let a = with_scheme(&profile("Sample"), &mine.id);
    let (mut app, paths, wid, ids) = app_with(std::slice::from_ref(&a), "", &[]);
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "graphite");
    assert_eq!(app.notices.len(), 1, "{:?}", app.notices);

    save_user(&paths.schemes(), &mine).unwrap();
    let _ = app.update(Message::ConfigChanged(SCHEMES_CHANGED));
    assert!(app.schemes.get(&mine.id).is_some());
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, mine.id);
    assert!(answers(&term(&app, wid, ids[0]), rgb(0x101820)));
    assert!(app.notices.is_empty(), "{:?}", app.notices);

    // A file that does not load is reported, and its panes fall back.
    std::fs::write(
        paths.schemes().join(format!("{}.toml", mine.id)),
        "id = 1\n",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(SCHEMES_CHANGED));
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "graphite");
    assert!(answers(&term(&app, wid, ids[0]), rgb(0x0b0b0e)));
    assert!(
        app.notices.iter().any(|n| n.contains(&mine.id)),
        "{:?}",
        app.notices
    );
}

#[cfg(unix)]
mod unix {
    use fbterm_term::TerminalOutput;

    use super::*;
    use crate::sessions::pane_tests::unix::{connected, screen, shell, wait_for};

    /// A scheme that gains a font changes its panes' font; the view re-measures and reports
    /// the new grid, which reaches the session once, through the resize debounce.
    #[test]
    fn saving_a_scheme_font_resizes_its_panes_once() {
        let mine = user_scheme("Mine", 0x101820);
        let paths = paths_with(&[]);
        save_user(&paths.schemes(), &mine).unwrap();
        std::fs::write(
            paths.settings(),
            format!("color_scheme = \"{}\"\nfont_size = 13.0\n", mine.id),
        )
        .unwrap();
        let (mut app, _) = App::build(paths.clone(), true);
        app.state_saving_enabled = false;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.05; done";
        let first = connected("one", shell(script, tx.clone()));
        let mut second = connected("two", shell("sleep 30", tx));
        second.scheme = Some("graphite".into());
        let (a, b) = (first.id, second.id);
        let term_a = first.terminal().unwrap().clone();
        let mut tab = Tab::single(first);
        tab.layout.split(a, Side::Right, b);
        tab.panes.push(second);
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(tab);
        app.windows.clear();
        app.windows.insert(wid, w);
        wait_for(&term_a, "ready");
        assert_eq!(style(&app, wid, a).font_size, 13.0);

        let sized = Scheme {
            font_size: Some(20.0),
            ..mine
        };
        save_user(&paths.schemes(), &sized).unwrap();
        let _ = app.update(Message::ConfigChanged(SCHEMES_CHANGED));
        assert_eq!(style(&app, wid, a).font_size, 20.0);
        assert_eq!(
            style(&app, wid, b).font_size,
            13.0,
            "other scheme untouched"
        );
        assert!(
            app.pending_resizes.is_empty(),
            "no resize of its own: the view re-measures"
        );

        // What the view reports after measuring the bigger cell.
        let bounds = iced::Size::new(800.0, 400.0);
        let cell = |font: f32| iced::Size::new(font * 8.0 / 13.0, (font * 1.2).ceil());
        let before = fbterm_term::grid_size_from_bounds(bounds, cell(13.0));
        let after = fbterm_term::grid_size_from_bounds(bounds, cell(20.0));
        assert_ne!(before, after);
        let _ = app.update(Message::Terminal(wid, a, TerminalOutput::Resized(after)));
        let generation = app.pending_resizes[&a].0;
        assert!(!app.pending_resizes.contains_key(&b));
        let _ = app.update(Message::ResizeSettled(a, generation));
        assert!(app.pending_resizes.is_empty());
        let size = format!("{} {}", after.rows, after.cols);
        wait_for(&term_a, &size);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            screen(&term_a).matches(&size).count(),
            1,
            "one session resize"
        );
    }
}
