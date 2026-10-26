//! The Color schemes window: its state rules, and against a temporary config directory, what
//! Save, Duplicate, Rename and Delete do to files, profiles, settings and open panes.

use fbterm_config::schemes::{Scheme, SchemeSet, read_file};
use fbterm_config::{ProfileStore, Settings};

use super::{CONTRAST_HINT, DeleteAsk, Entry, ImportChoice, SchemesMsg, SchemesState};
use crate::app::{App, Message};
use crate::editor::EditorMsg;
use crate::keys::Action;
use crate::schemes_tests::{
    answers, app_with, builtin, generation, pane, rgb, style, user_scheme, with_scheme,
};
use crate::settings_window::SettingsMsg;

fn state_with(schemes: &[Scheme], select: &str) -> SchemesState {
    let dir = std::env::temp_dir().join(format!(
        "fbterm-schemes-window-{}",
        fbterm_config::ProfileId::new()
    ));
    for s in schemes {
        fbterm_config::schemes::save_user(&dir, s).unwrap();
    }
    let (set, errors) = SchemeSet::load(&dir);
    assert!(errors.is_empty(), "{errors:?}");
    SchemesState::new(&set, Some(select), Vec::new())
}

fn send(app: &mut App, m: SchemesMsg) {
    let _ = app.update(Message::Schemes(m));
}

fn win(app: &App) -> &SchemesState {
    &app.schemes_win.as_ref().expect("schemes window open").1
}

/// Open the window on `id`.
fn open(app: &mut App, id: &str) {
    send(app, SchemesMsg::Open);
    send(app, SchemesMsg::Select(id.into()));
    assert_eq!(win(app).selected, id);
}

fn file(app: &App, id: &str) -> std::path::PathBuf {
    app.paths.schemes().join(format!("{id}.toml"))
}

#[test]
fn builtins_are_read_only() {
    let mut s = state_with(&[], "harbor");
    let harbor = builtin("harbor");
    assert!(!s.editable());
    s.update(SchemesMsg::Color(Entry::Background, rgb(0x123456)));
    s.update(SchemesMsg::Hex(Entry::Foreground, "#654321".into()));
    s.update(SchemesMsg::RenameStart);
    s.update(SchemesMsg::Rename("Mine".into()));
    s.update(SchemesMsg::FontSize(Some(20.0)));
    assert_eq!(s.selected_draft(), Some(&harbor));
    assert!(!s.renaming);
    assert!(!s.is_dirty("harbor"));
    assert_eq!(s.hex_text(Entry::Background), "#0a1a20");
    // Selecting still works, and so does showing another entry.
    s.update(SchemesMsg::Select("ember".into()));
    assert_eq!(s.selected, "ember");
    assert!(s.contrast_hint().is_none(), "built-ins pass");
}

#[test]
fn duplicate_makes_editable_copy() {
    let a = fbterm_config_profile("a");
    let (mut app, _, _, _) = app_with(&[a], "", &[]);
    open(&mut app, "harbor");
    send(&mut app, SchemesMsg::Duplicate);
    let s = win(&app);
    let id = s.selected.clone();
    assert!(id.starts_with("user-"), "{id}");
    let copy = s.selected_draft().unwrap().clone();
    assert_eq!(copy.name, "Harbor copy");
    assert!(!copy.builtin);
    assert_eq!(copy.colors, builtin("harbor").colors);
    assert!(s.editable());
    assert!(!s.is_dirty(&id), "the copy is saved at once");
    // Written to schemes/ and loaded.
    let on_disk = read_file(&file(&app, &id)).unwrap();
    assert_eq!(on_disk.name, "Harbor copy");
    assert_eq!(
        app.schemes.get(&id).map(|s| s.name.as_str()),
        Some("Harbor copy")
    );
    // Now its colors change.
    send(&mut app, SchemesMsg::Color(Entry::Cursor, rgb(0x00ff00)));
    assert_eq!(
        win(&app).selected_draft().unwrap().colors.cursor,
        rgb(0x00ff00)
    );
}

#[test]
fn hex_input_validates() {
    let mine = user_scheme("Mine", 0x101010);
    let mut s = state_with(std::slice::from_ref(&mine), &mine.id);
    assert!(s.editable());
    // Not a color yet: the field keeps the text, the color stays.
    s.update(SchemesMsg::Hex(Entry::Background, "#12345".into()));
    assert_eq!(s.hex_text(Entry::Background), "#12345");
    assert!(!s.hex_valid(Entry::Background));
    assert_eq!(s.selected_draft().unwrap().colors.background, rgb(0x101010));
    s.update(SchemesMsg::Hex(Entry::Background, "12345g".into()));
    assert_eq!(s.selected_draft().unwrap().colors.background, rgb(0x101010));
    // A color: applied, and its sliders show.
    s.update(SchemesMsg::Hex(Entry::Ansi(3), "#123456".into()));
    assert!(s.hex_valid(Entry::Ansi(3)));
    assert_eq!(s.selected_draft().unwrap().colors.ansi[3], rgb(0x123456));
    assert_eq!(s.editing, Entry::Ansi(3));
    // A slider writes the field.
    s.update(SchemesMsg::Color(Entry::Background, rgb(0xabcdef)));
    assert_eq!(s.hex_text(Entry::Background), "#abcdef");
    assert!(s.is_dirty(&mine.id));
    assert_eq!(Entry::Ansi(9).label(), "Bright red");
}

#[test]
fn edits_preview_without_touching_sessions() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let (mut app, _, wid, ids) = app_with(&[a], "", std::slice::from_ref(&mine));
    let before = style(&app, wid, ids[0]);
    let gen_before = generation(&app, wid, ids[0]);
    open(&mut app, &mine.id);
    send(
        &mut app,
        SchemesMsg::Color(Entry::Background, rgb(0x203040)),
    );
    send(
        &mut app,
        SchemesMsg::Hex(Entry::Foreground, "#fefefe".into()),
    );
    // The preview shows the edit...
    let preview = win(&app).preview_style(&app.style);
    assert_eq!(preview.scheme.background, rgb(0x203040));
    assert_eq!(preview.scheme.foreground, rgb(0xfefefe));
    // ...and nothing else does: not the pane, its terminal, the set or the file.
    assert_eq!(style(&app, wid, ids[0]), before);
    assert_eq!(generation(&app, wid, ids[0]), gen_before);
    assert_eq!(app.schemes.get(&mine.id), Some(&mine));
    assert_eq!(read_file(&file(&app, &mine.id)).unwrap(), mine);
    assert!(answers(
        &crate::schemes_tests::term(&app, wid, ids[0]),
        rgb(0x101010)
    ));
}

#[test]
fn save_writes_and_reapplies() {
    let mine = user_scheme("Mine", 0x101010);
    let other = user_scheme("Other", 0x202020);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let b = with_scheme(&fbterm_config_profile("b"), &other.id);
    let (mut app, _, wid, ids) = app_with(&[a, b], "", &[mine.clone(), other.clone()]);
    let untouched = generation(&app, wid, ids[1]);
    open(&mut app, &mine.id);
    send(
        &mut app,
        SchemesMsg::Color(Entry::Background, rgb(0x203040)),
    );
    send(&mut app, SchemesMsg::FontSize(Some(15.0)));
    send(&mut app, SchemesMsg::Save);
    let saved = read_file(&file(&app, &mine.id)).unwrap();
    assert_eq!(saved.colors.background, rgb(0x203040));
    assert_eq!(saved.font_size, Some(15.0));
    assert_eq!(app.schemes.get(&mine.id), Some(&saved));
    assert!(!win(&app).is_dirty(&mine.id));
    assert_eq!(win(&app).notice.as_deref(), Some("Saved"));
    // The pane using it is redrawn with it and answers programs with it.
    let s = style(&app, wid, ids[0]);
    assert_eq!(s.scheme.background, rgb(0x203040));
    assert_eq!(s.font_size, 15.0);
    let t = crate::schemes_tests::term(&app, wid, ids[0]);
    assert!(answers(&t, rgb(0x203040)));
    // The other pane is left alone.
    assert_eq!(generation(&app, wid, ids[1]), untouched);
    assert_eq!(
        app.scheme_for(pane(&app, wid, ids[1])).colors.background,
        rgb(0x202020)
    );
}

#[test]
fn rename_keeps_references() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let (mut app, paths, wid, ids) = app_with(&[a], "", std::slice::from_ref(&mine));
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::RenameStart);
    assert!(win(&app).renaming);
    send(&mut app, SchemesMsg::Rename("Night shift".into()));
    send(&mut app, SchemesMsg::RenameEnd);
    send(&mut app, SchemesMsg::Save);
    // Same id, new name: the profile still resolves to it.
    let saved = read_file(&file(&app, &mine.id)).unwrap();
    assert_eq!(saved.name, "Night shift");
    let store = ProfileStore::load(&paths.profiles()).unwrap();
    assert_eq!(
        store.profiles[0].color_scheme.as_deref(),
        Some(mine.id.as_str())
    );
    let used = app.scheme_for(pane(&app, wid, ids[0]));
    assert_eq!(used.id, mine.id);
    assert_eq!(used.name, "Night shift");
    assert!(
        app.notices.iter().all(|n| !n.contains("not found")),
        "{:?}",
        app.notices
    );
    // An empty name is not saved.
    send(&mut app, SchemesMsg::RenameStart);
    send(&mut app, SchemesMsg::Rename("  ".into()));
    send(&mut app, SchemesMsg::Save);
    assert!(win(&app).error.is_some());
    assert_eq!(
        read_file(&file(&app, &mine.id)).unwrap().name,
        "Night shift"
    );
}

#[test]
fn delete_asks_with_profile_count() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let b = with_scheme(&fbterm_config_profile("b"), &mine.id);
    let c = fbterm_config_profile("c");
    let (mut app, _, _, _) = app_with(&[a, b, c], "", std::slice::from_ref(&mine));
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::Delete);
    let ask = win(&app).confirm.clone().expect("asks first");
    assert_eq!(ask.profiles, 2);
    assert_eq!(ask.title(), "Delete \"Mine\"?");
    assert_eq!(
        ask.body().as_deref(),
        Some("2 profiles use it and will switch to the global scheme.")
    );
    // Cancel keeps it.
    send(&mut app, SchemesMsg::CancelDelete);
    assert!(win(&app).confirm.is_none());
    assert!(file(&app, &mine.id).exists());
    // The copy for one profile and for none.
    let one = DeleteAsk {
        profiles: 1,
        ..ask.clone()
    };
    assert_eq!(
        one.body().as_deref(),
        Some("1 profile uses it and will switch to the global scheme.")
    );
    let none = DeleteAsk { profiles: 0, ..ask };
    assert_eq!(none.body(), None);
    // Built-ins cannot be deleted.
    send(&mut app, SchemesMsg::Select("graphite".into()));
    send(&mut app, SchemesMsg::Delete);
    assert!(win(&app).confirm.is_none());
}

#[test]
fn delete_resets_profiles_and_global() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let b = with_scheme(&fbterm_config_profile("b"), "harbor");
    let settings = format!("color_scheme = \"{}\"\n", mine.id);
    let (mut app, paths, wid, ids) = app_with(&[a, b], &settings, std::slice::from_ref(&mine));
    assert_eq!(app.settings.color_scheme, mine.id);
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::Delete);
    send(&mut app, SchemesMsg::ConfirmDelete);
    assert!(win(&app).confirm.is_none());
    // The file is gone, and so is the scheme.
    assert!(!file(&app, &mine.id).exists());
    assert!(app.schemes.get(&mine.id).is_none());
    assert_ne!(win(&app).selected, mine.id);
    assert!(win(&app).draft(&mine.id).is_none());
    // Its profile follows the global scheme, saved; the other keeps its own.
    let store = ProfileStore::load(&paths.profiles()).unwrap();
    assert_eq!(store.profiles[0].color_scheme, None);
    assert_eq!(store.profiles[1].color_scheme.as_deref(), Some("harbor"));
    assert_eq!(app.profiles.profiles[0].color_scheme, None);
    // The global setting is Graphite again, saved.
    assert_eq!(app.settings.color_scheme, "graphite");
    assert_eq!(
        Settings::load(&paths.settings()).unwrap().color_scheme,
        "graphite"
    );
    // The pane is Graphite now, with no "not found" line.
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, "graphite");
    assert!(
        app.notices.iter().all(|n| !n.contains("not found")),
        "{:?}",
        app.notices
    );
    // Our own writes are not edits to pick up again.
    let notices = app.notices.clone();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: true,
        settings: true,
        schemes: true,
    }));
    assert_eq!(app.notices, notices);
}

#[test]
fn contrast_hint_shows_for_low_contrast() {
    let mut low = user_scheme("Low", 0x101010);
    low.colors.foreground = rgb(0x202020);
    let fine = user_scheme("Fine", 0x0b0b0e);
    let mut s = state_with(&[low.clone(), fine.clone()], &low.id);
    assert_eq!(s.contrast_hint(), Some(CONTRAST_HINT));
    assert_eq!(
        CONTRAST_HINT,
        "Text may be hard to read on this background."
    );
    s.update(SchemesMsg::Select(fine.id.clone()));
    assert_eq!(s.contrast_hint(), None);
    // An edit that hurts shows it at once (it never blocks Save).
    s.update(SchemesMsg::Color(Entry::Foreground, rgb(0x111111)));
    assert_eq!(s.contrast_hint(), Some(CONTRAST_HINT));
}

#[test]
fn window_opens_from_settings_palette_and_editor() {
    let a = fbterm_config_profile("a");
    let (mut app, _, wid, _) = app_with(&[a], "", &[]);
    let close = |app: &mut App| {
        let id = app.schemes_window().unwrap();
        send(app, SchemesMsg::Close);
        let _ = app.update(Message::WindowClosed(id));
        assert!(app.schemes_window().is_none());
    };
    // Settings' `Edit schemes…`, on the global scheme.
    let _ = app.update(Message::Settings(SettingsMsg::Open));
    let _ = app.update(Message::Settings(SettingsMsg::EditSchemes));
    let id = app.schemes_window().expect("opened from Settings");
    assert_eq!(app.title(id), "Color schemes · fbterm");
    assert_eq!(win(&app).selected, "graphite");
    close(&mut app);
    // The palette's `Color schemes` action.
    let listed = crate::palette::items(&app.profiles).into_iter().any(|i| {
        matches!(i, crate::palette::PaletteItem::Action { action: Action::EditSchemes, ref label } if label == "Color schemes")
    });
    assert!(listed);
    assert_eq!(Action::EditSchemes.title(), "Color schemes");
    let _ = app.update(Message::Shortcut(wid, Action::EditSchemes));
    assert!(app.schemes_window().is_some(), "opened from the palette");
    close(&mut app);
    // The profile editor's `Edit schemes…`, on the selected profile's scheme.
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let pid = app.profiles.profiles[0].id;
    let _ = app.update(Message::Editor(EditorMsg::Select(pid)));
    let _ = app.update(Message::Editor(EditorMsg::ColorScheme(Some(
        "ember".into(),
    ))));
    let _ = app.update(Message::Editor(EditorMsg::EditSchemes));
    assert!(app.schemes_window().is_some(), "opened from the editor");
    assert_eq!(win(&app).selected, "ember");
    // Asking again brings it forward with that scheme selected; it is still one window.
    let first = app.schemes_window();
    let _ = app.update(Message::Settings(SettingsMsg::EditSchemes));
    assert_eq!(app.schemes_window(), first);
    assert_eq!(win(&app).selected, "graphite");
    // The app keeps running while only this window is open.
    let _ = app.update(Message::WindowClosed(wid));
    assert!(app.schemes_window().is_some());
}

#[test]
fn unknown_scheme_id_displays_not_found() {
    let a = fbterm_config_profile("a");
    let (app, _, _, _) = app_with(&[a], "", &[]);
    let with_global = app.scheme_choices(true);
    let plain = app.scheme_choices(false);
    // A known id shows its name, `None` the global entry, an unknown id `Scheme not found`
    // (and nothing is selected, so the id is never rewritten).
    assert_eq!(crate::schemes::shown(&plain, Some("harbor")), "Harbor");
    assert_eq!(
        crate::schemes::shown(&with_global, None),
        crate::schemes::USE_GLOBAL
    );
    assert_eq!(
        crate::schemes::shown(&plain, Some("user-gone")),
        "Scheme not found"
    );
    assert_eq!(
        crate::schemes::shown(&with_global, Some("user-gone")),
        crate::schemes::NOT_FOUND
    );
    assert!(crate::schemes::chosen(&plain, Some("user-gone")).is_none());
}

#[test]
fn reload_keeps_unsaved_edits_and_picks_up_new_files() {
    let mine = user_scheme("Mine", 0x101010);
    let a = fbterm_config_profile("a");
    let (mut app, paths, _, _) = app_with(&[a], "", std::slice::from_ref(&mine));
    open(&mut app, &mine.id);
    send(
        &mut app,
        SchemesMsg::Color(Entry::Background, rgb(0x203040)),
    );
    let added = user_scheme("Added", 0x303030);
    fbterm_config::schemes::save_user(&paths.schemes(), &added).unwrap();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: false,
        settings: false,
        schemes: true,
    }));
    let s = win(&app);
    assert!(s.draft(&added.id).is_some());
    assert_eq!(s.selected_draft().unwrap().colors.background, rgb(0x203040));
    assert!(s.is_dirty(&mine.id));
}

#[test]
fn preview_shows_the_sample_with_a_selected_word() {
    let s = state_with(&[], "graphite");
    let term = s.preview.lock();
    let sel = term.selection().expect("a word is selected");
    assert_eq!(sel.text(&term), "README.md");
}

/// A local profile named `name` (a shell that is never started).
fn fbterm_config_profile(name: &str) -> fbterm_config::Profile {
    crate::restore_tests::profile(name)
}

/// Makes a directory read-only while alive (so writes into it fail), writable again after.
#[cfg(unix)]
struct ReadOnly(std::path::PathBuf);

#[cfg(unix)]
impl ReadOnly {
    fn new(dir: &std::path::Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        Self(dir.to_path_buf())
    }
}

#[cfg(unix)]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn delete_with_profiles_write_failure_keeps_scheme_and_references() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let settings = format!("color_scheme = \"{}\"\n", mine.id);
    let (mut app, paths, wid, ids) = app_with(&[a], &settings, std::slice::from_ref(&mine));
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::Delete);
    {
        // profiles.toml (and settings.toml) cannot be written; schemes/ still could be.
        let _guard = ReadOnly::new(&paths.config_dir);
        send(&mut app, SchemesMsg::ConfirmDelete);
    }
    let s = win(&app);
    assert!(
        s.error
            .as_deref()
            .is_some_and(|e| e.contains("profiles.toml")),
        "{:?}",
        s.error
    );
    assert!(s.notice.is_none());
    // Nothing moved: the file, the set, the draft, the profiles and the global.
    assert!(file(&app, &mine.id).exists());
    assert_eq!(app.schemes.get(&mine.id), Some(&mine));
    assert_eq!(s.selected, mine.id);
    let id = Some(mine.id.as_str());
    assert_eq!(app.profiles.profiles[0].color_scheme.as_deref(), id);
    let store = ProfileStore::load(&paths.profiles()).unwrap();
    assert_eq!(store.profiles[0].color_scheme.as_deref(), id);
    assert_eq!(app.settings.color_scheme, mine.id);
    assert_eq!(
        Settings::load(&paths.settings()).unwrap().color_scheme,
        mine.id
    );
    assert_eq!(app.scheme_for(pane(&app, wid, ids[0])).id, mine.id);
    // Once the folder is writable again, the delete goes through.
    send(&mut app, SchemesMsg::Delete);
    send(&mut app, SchemesMsg::ConfirmDelete);
    assert!(!file(&app, &mine.id).exists());
    assert_eq!(app.settings.color_scheme, "graphite");
}

#[cfg(unix)]
#[test]
fn delete_with_settings_write_failure_keeps_scheme_file() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let paths = crate::restore_tests::paths_with(std::slice::from_ref(&a));
    fbterm_config::schemes::save_user(&paths.schemes(), &mine).unwrap();
    // settings.toml is a link into a folder that turns read-only; settings are written to
    // the link's target.
    let elsewhere = paths.config_dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("settings.toml");
    std::fs::write(&target, format!("color_scheme = \"{}\"\n", mine.id)).unwrap();
    std::os::unix::fs::symlink(&target, paths.settings()).unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    app.state_saving_enabled = false;
    assert_eq!(app.settings.color_scheme, mine.id);
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::Delete);
    {
        let _guard = ReadOnly::new(&elsewhere);
        send(&mut app, SchemesMsg::ConfirmDelete);
    }
    let s = win(&app);
    assert!(
        s.error
            .as_deref()
            .is_some_and(|e| e.contains("settings.toml")),
        "{:?}",
        s.error
    );
    // The scheme stays, and so does the global that names it; the profile, written first,
    // follows the global scheme (which is still this one), so nothing refers to a missing id.
    assert!(file(&app, &mine.id).exists());
    assert!(app.schemes.get(&mine.id).is_some());
    assert_eq!(app.settings.color_scheme, mine.id);
    assert_eq!(Settings::load(&target).unwrap().color_scheme, mine.id);
    assert_eq!(app.profiles.profiles[0].color_scheme, None);
    let store = ProfileStore::load(&paths.profiles()).unwrap();
    assert_eq!(store.profiles[0].color_scheme, None);
    assert!(
        app.notices.iter().all(|n| !n.contains("not found")),
        "{:?}",
        app.notices
    );
}

#[cfg(unix)]
#[test]
fn delete_updates_editor_drafts_only_after_save() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let pid = a.id;
    let (mut app, paths, _, _) = app_with(&[a], "", std::slice::from_ref(&mine));
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let draft_scheme = |app: &App| {
        let (_, ed) = app.editor.as_ref().unwrap();
        ed.draft(pid).unwrap().color_scheme.clone()
    };
    assert_eq!(draft_scheme(&app), Some(mine.id.clone()));
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::Delete);
    {
        let _guard = ReadOnly::new(&paths.config_dir);
        send(&mut app, SchemesMsg::ConfirmDelete);
    }
    assert!(win(&app).error.is_some());
    assert_eq!(
        draft_scheme(&app),
        Some(mine.id.clone()),
        "a failed save leaves the drafts alone"
    );
    send(&mut app, SchemesMsg::Delete);
    send(&mut app, SchemesMsg::ConfirmDelete);
    assert!(win(&app).error.is_none());
    assert_eq!(draft_scheme(&app), None);
}

#[test]
fn preview_resize_does_not_loop() {
    use iced::advanced::layout::{self, Layout};
    use iced::advanced::widget::Tree;
    use iced::advanced::{Shell, clipboard};
    use iced::{Element, Event, Pixels, Rectangle, Size, mouse};

    let a = fbterm_config_profile("a");
    let (mut app, _, _, _) = app_with(&[a], "", &[]);
    open(&mut app, "graphite");
    let size = Size::new(520.0, 116.0);
    let base = app.style.clone();
    let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        Pixels(16.0),
    ));
    // Lay the preview out once at `size` and pass it an event: what does it publish?
    let run = |state: &SchemesState| -> Vec<Message> {
        let mut element: Element<'_, Message> = fbterm_term::highlighted_terminal_view(
            &state.preview,
            state.preview_style(&base),
            None,
            |out| Message::Schemes(SchemesMsg::Preview(out)),
        );
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::CursorEntered),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(size),
        );
        messages
    };
    let resized = |messages: &[Message]| {
        messages
            .iter()
            .filter_map(|m| match m {
                Message::Schemes(SchemesMsg::Preview(fbterm_term::TerminalOutput::Resized(g))) => {
                    Some(*g)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // The first layout differs from the default grid: one resize.
    let first = resized(&run(win(&app)));
    assert_eq!(first.len(), 1, "{first:?}");
    let grid = first[0];
    send(
        &mut app,
        SchemesMsg::Preview(fbterm_term::TerminalOutput::Resized(grid)),
    );
    // The rebuilt terminal has that grid, the sample and its selection...
    {
        let term = win(&app).preview.lock();
        assert_eq!(term.size(), grid);
        assert_eq!(term.selection().unwrap().text(&term), "README.md");
    }
    // ...so laying it out the same way again reports nothing.
    assert!(resized(&run(win(&app))).is_empty());
}

/// A scratch directory inside the app's config directory, for files outside `schemes/`.
fn scratch(app: &App) -> std::path::PathBuf {
    let dir = app.paths.schemes().parent().unwrap().join("scratch");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What the chosen file does: read as the blocking task does, then handled.
fn import_file(app: &mut App, path: std::path::PathBuf) {
    let read = read_file(&path).map_err(|e| e.to_string());
    send(app, SchemesMsg::ImportRead(read));
}

fn files_in_schemes(app: &App) -> usize {
    std::fs::read_dir(app.paths.schemes())
        .map(|d| d.count())
        .unwrap_or(0)
}

#[test]
fn export_writes_a_loadable_file() {
    let mine = user_scheme("Mine", 0x101010);
    let (mut app, _, _, _) = app_with(
        &[fbterm_config_profile("a")],
        "",
        std::slice::from_ref(&mine),
    );
    open(&mut app, &mine.id);
    let out = scratch(&app).join("mine.toml");
    send(&mut app, SchemesMsg::Exported(Some(out.clone())));
    assert_eq!(read_file(&out).unwrap(), mine);
    assert_eq!(win(&app).notice.as_deref(), Some("Exported"));
    // A built-in exports too.
    send(&mut app, SchemesMsg::Select("harbor".into()));
    let out = scratch(&app).join("harbor.toml");
    send(&mut app, SchemesMsg::Exported(Some(out.clone())));
    let back = read_file(&out).unwrap();
    assert_eq!(back.id, "harbor");
    assert_eq!(back.colors, builtin("harbor").colors);
    // Cancelling the dialog does nothing.
    send(&mut app, SchemesMsg::Exported(None));
    assert!(win(&app).error.is_none());
}

#[test]
fn import_copies_into_schemes_dir() {
    let other = user_scheme("Elsewhere", 0x303030);
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    let src = scratch(&app).join("in.toml");
    fbterm_config::schemes::write_file(&src, &other).unwrap();
    import_file(&mut app, src);
    assert_eq!(read_file(&file(&app, &other.id)).unwrap(), other);
    assert_eq!(app.schemes.get(&other.id), Some(&other));
    assert_eq!(win(&app).selected, other.id);
    assert!(win(&app).error.is_none());
    assert!(win(&app).import.is_none());
}

#[test]
fn import_invalid_file_shows_error() {
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    let src = scratch(&app).join("bad.toml");
    std::fs::write(
        &src,
        "id = \"x\"\nname = \"X\"\n\n[colors]\nforeground = 5\n",
    )
    .unwrap();
    import_file(&mut app, src.clone());
    let err = win(&app).error.clone().expect("error shown");
    assert!(err.contains("bad.toml"), "{err}");
    assert!(err.contains("bad.toml:5"), "{err}");
    assert_eq!(files_in_schemes(&app), 0);
    assert_eq!(win(&app).selected, "graphite");
    // A file that does not exist is an error too.
    import_file(&mut app, src.with_file_name("none.toml"));
    assert!(win(&app).error.as_deref().unwrap().contains("none.toml"));
}

#[test]
fn import_builtin_id_gets_new_id() {
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    let mut s = builtin("harbor");
    s.builtin = false;
    let src = scratch(&app).join("harbor.toml");
    fbterm_config::schemes::write_file(&src, &s).unwrap();
    import_file(&mut app, src);
    assert!(win(&app).import.is_none(), "no question for a built-in id");
    let id = win(&app).selected.clone();
    assert!(id.starts_with("user-"), "{id}");
    let got = app.schemes.get(&id).expect("loaded");
    assert!(!got.builtin);
    assert_eq!(got.colors, builtin("harbor").colors);
    assert_eq!(got.name, "Harbor");
    // The built-in is untouched.
    assert!(app.schemes.get("harbor").unwrap().builtin);
}

#[test]
fn import_invalid_id_gets_new_id() {
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    for bad in ["Has Upper", "bad/slash", &"a".repeat(101)] {
        let mut s = user_scheme("Odd", 0x404040);
        s.id = bad.to_owned();
        let src = scratch(&app).join("odd.toml");
        fbterm_config::schemes::write_file(&src, &s).unwrap();
        import_file(&mut app, src);
        assert!(win(&app).error.is_none(), "{bad}: {:?}", win(&app).error);
        let id = win(&app).selected.clone();
        assert!(id.starts_with("user-"), "{bad} -> {id}");
        assert_eq!(app.schemes.get(&id).unwrap().name, "Odd");
    }
    assert_eq!(files_in_schemes(&app), 3);
}

#[test]
fn import_same_id_keeps_both_with_new_id() {
    let mine = user_scheme("Mine", 0x101010);
    let (mut app, _, _, _) = app_with(
        &[fbterm_config_profile("a")],
        "",
        std::slice::from_ref(&mine),
    );
    open(&mut app, "graphite");
    let mut incoming = mine.clone();
    incoming.colors.background = rgb(0x505050);
    let src = scratch(&app).join("same.toml");
    fbterm_config::schemes::write_file(&src, &incoming).unwrap();
    import_file(&mut app, src);
    assert!(win(&app).import.is_some(), "asks first");
    assert_eq!(files_in_schemes(&app), 1);
    send(&mut app, SchemesMsg::ImportChoice(ImportChoice::KeepBoth));
    assert!(win(&app).import.is_none());
    assert_eq!(files_in_schemes(&app), 2);
    assert_eq!(app.schemes.get(&mine.id), Some(&mine));
    let id = win(&app).selected.clone();
    assert_ne!(id, mine.id);
    let got = app.schemes.get(&id).unwrap();
    assert_eq!(got.name, "Mine (imported)");
    assert_eq!(got.colors.background, rgb(0x505050));
    // Cancel imports nothing.
    let src = scratch(&app).join("same.toml");
    import_file(&mut app, src);
    send(&mut app, SchemesMsg::ImportChoice(ImportChoice::Cancel));
    assert!(win(&app).import.is_none());
    assert_eq!(files_in_schemes(&app), 2);
}

#[test]
fn import_same_id_replace_updates_and_reapplies() {
    let mine = user_scheme("Mine", 0x101010);
    let a = with_scheme(&fbterm_config_profile("a"), &mine.id);
    let (mut app, _, wid, ids) = app_with(&[a], "", std::slice::from_ref(&mine));
    open(&mut app, "graphite");
    let mut incoming = mine.clone();
    incoming.name = "Mine v2".into();
    incoming.colors.background = rgb(0x505050);
    let src = scratch(&app).join("same.toml");
    fbterm_config::schemes::write_file(&src, &incoming).unwrap();
    import_file(&mut app, src);
    send(&mut app, SchemesMsg::ImportChoice(ImportChoice::Replace));
    assert_eq!(files_in_schemes(&app), 1);
    assert_eq!(read_file(&file(&app, &mine.id)).unwrap(), incoming);
    assert_eq!(app.schemes.get(&mine.id), Some(&incoming));
    assert_eq!(win(&app).selected, mine.id);
    assert_eq!(style(&app, wid, ids[0]).scheme.background, rgb(0x505050));
}

#[test]
fn replace_import_over_dirty_draft_shows_imported() {
    let mine = user_scheme("Mine", 0x101010);
    let (mut app, _, _, _) = app_with(
        &[fbterm_config_profile("a")],
        "",
        std::slice::from_ref(&mine),
    );
    open(&mut app, &mine.id);
    send(&mut app, SchemesMsg::Color(Entry::Cursor, rgb(0x00ff00)));
    assert!(win(&app).is_dirty(&mine.id));
    let mut incoming = mine.clone();
    incoming.name = "Mine v2".into();
    incoming.colors.background = rgb(0x505050);
    let src = scratch(&app).join("same.toml");
    fbterm_config::schemes::write_file(&src, &incoming).unwrap();
    import_file(&mut app, src);
    send(&mut app, SchemesMsg::ImportChoice(ImportChoice::Replace));
    assert_eq!(win(&app).draft(&mine.id), Some(&incoming));
    assert!(!win(&app).is_dirty(&mine.id));
}

#[test]
fn import_oversized_file_is_refused() {
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    let src = scratch(&app).join("big.toml");
    std::fs::write(&src, " ".repeat(70 * 1024)).unwrap();
    import_file(&mut app, src);
    let err = win(&app).error.clone().expect("error shown");
    assert!(
        err.contains("Not a color scheme file (too large)."),
        "{err}"
    );
    assert_eq!(files_in_schemes(&app), 0);
}

#[test]
fn import_binary_file_is_refused() {
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    let src = scratch(&app).join("bin.toml");
    std::fs::write(&src, [0xff, 0xfe, 0x00, 0x80, 0xc3, 0x28]).unwrap();
    import_file(&mut app, src);
    let err = win(&app).error.clone().expect("error shown");
    assert!(err.contains("Not a color scheme file"), "{err}");
    assert_eq!(files_in_schemes(&app), 0);
}

#[test]
fn import_over_unloaded_file_with_same_id_asks() {
    let (mut app, _, _, _) = app_with(&[fbterm_config_profile("a")], "", &[]);
    open(&mut app, "graphite");
    let s = user_scheme("Mine", 0x101010);
    std::fs::create_dir_all(app.paths.schemes()).unwrap();
    std::fs::write(file(&app, &s.id), "not a scheme").unwrap();
    let src = scratch(&app).join("in.toml");
    fbterm_config::schemes::write_file(&src, &s).unwrap();
    import_file(&mut app, src);
    assert!(win(&app).import.is_some());
    assert_eq!(
        std::fs::read_to_string(file(&app, &s.id)).unwrap(),
        "not a scheme"
    );
}
