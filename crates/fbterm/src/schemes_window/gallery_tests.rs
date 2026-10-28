//! The theme gallery in the Color schemes window, against a temporary config directory and a
//! fake fetch: when the catalog is fetched, how failures read, and what Install and Update do
//! to scheme files and open panes. Nothing here touches the network.

use std::sync::Arc;

use fbterm_config::catalog::{CATALOG_URL, Catalog, MAX_CATALOG_BYTES, parse_catalog};
use fbterm_config::schemes::{Scheme, read_file};
use iced::Task;
use parking_lot::Mutex;

use super::gallery::{
    EDITED_HERE, GalleryIo, GalleryLoad, GalleryRow, NO_LONGER_OFFERED, RowButton, UpdateChoice,
    clip_chars, credit_line, gallery_rows,
};
use super::{Entry, SchemesMsg, SchemesState};
use crate::app::{App, Message};
use crate::schemes_tests::{app_with, rgb, style, with_scheme};

/// What the fake fetch and opener saw.
#[derive(Default)]
struct Calls {
    fetched: Vec<(String, usize)>,
    opened: Vec<String>,
}

/// Replace the app's network fetch and link opener with fakes: the fetch answers `answer`.
fn fake_io(app: &mut App, answer: Result<Vec<u8>, String>) -> Arc<Mutex<Calls>> {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let (c1, c2) = (calls.clone(), calls.clone());
    app.gallery_io = GalleryIo {
        fetch: Arc::new(move |url, max| {
            c1.lock().fetched.push((url.to_owned(), max));
            answer.clone()
        }),
        open: Arc::new(move |url| c2.lock().opened.push(url.to_owned())),
    };
    calls
}

/// One catalog entry: Graphite-like colors on background `bg`.
fn entry(id: &str, version: u32, bg: &str, withdrawn: bool) -> String {
    format!(
        r##"
[[themes]]
id = "{id}"
name = "Theme {id}"
version = {version}
variant = "dark"
withdrawn = {withdrawn}

[themes.colors]
foreground = "#f5f5f7"
background = "{bg}"
cursor = "#a584ff"
selection = "#392f56"
black = "#262629"
red = "#ed4a49"
green = "#2bbb71"
yellow = "#f2a635"
blue = "#70b3f7"
magenta = "#a584ff"
cyan = "#59c5f5"
white = "#d0d0d9"
bright_black = "#919199"
bright_red = "#fd736d"
bright_green = "#52cd86"
bright_yellow = "#fac053"
bright_blue = "#9ccbfa"
bright_magenta = "#bea8ff"
bright_cyan = "#8fd6f7"
bright_white = "#f5f5f7"

[themes.credit]
project = "Project {id}"
author = "Someone"
url = "https://example.org/{id}"
license = "MIT"
copyright = "Copyright (c) 2026 Someone"
license_file = "LICENSES/{id}-MIT.txt"
description = "A theme."
derived = false
"##
    )
}

fn catalog(entries: &[String]) -> Vec<u8> {
    format!("format = 1\n{}", entries.concat()).into_bytes()
}

/// The scheme the gallery installs for the only theme of `bytes`.
fn installed_form(bytes: &[u8]) -> Scheme {
    parse_catalog(bytes).unwrap().themes.remove(0).scheme
}

fn send(app: &mut App, m: SchemesMsg) -> Task<Message> {
    app.update(Message::Schemes(m))
}

fn win(app: &App) -> &SchemesState {
    &app.schemes_win.as_ref().expect("schemes window open").1
}

fn file(app: &App, id: &str) -> std::path::PathBuf {
    app.paths.schemes().join(format!("{id}.toml"))
}

/// Run the fetch the way the blocking task does and hand its result to the window.
fn finish_fetch(app: &mut App) {
    let result = app.gallery_job()();
    let generation = app.gallery_generation;
    let _ = send(app, SchemesMsg::GalleryFetched(generation, result));
}

/// Open the window and its gallery, fetching `bytes` as the catalog.
fn open_gallery(app: &mut App, bytes: Vec<u8>) -> Arc<Mutex<Calls>> {
    let calls = fake_io(app, Ok(bytes));
    let _ = send(app, SchemesMsg::Open);
    let _ = send(app, SchemesMsg::ShowGallery(true));
    finish_fetch(app);
    assert!(matches!(win(app).gallery, GalleryLoad::Loaded(_)));
    calls
}

/// Load `bytes` as the next catalog, through Refresh.
fn refresh_with(app: &mut App, bytes: Vec<u8>) {
    let _ = fake_io(app, Ok(bytes));
    let _ = send(app, SchemesMsg::RefreshGallery);
    finish_fetch(app);
}

fn failed(app: &App) -> &str {
    match &win(app).gallery {
        GalleryLoad::Failed(line) => line,
        other => panic!("not failed: {other:?}"),
    }
}

/// A local profile named `name` (a shell that is never started).
fn profile(name: &str) -> fbterm_config::Profile {
    crate::restore_tests::profile(name)
}

fn scheme_files(app: &App) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(app.paths.schemes())
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn opening_gallery_fetches_once() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let calls = fake_io(&mut app, Ok(catalog(&[entry("x", 1, "#101010", false)])));
    let _ = send(&mut app, SchemesMsg::Open);
    assert_eq!(send(&mut app, SchemesMsg::ShowGallery(true)).units(), 1);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loading));
    assert!(win(&app).showing_gallery);
    // Away and back while it loads: no second fetch.
    let _ = send(&mut app, SchemesMsg::ShowGallery(false));
    assert_eq!(send(&mut app, SchemesMsg::ShowGallery(true)).units(), 0);
    assert_eq!(send(&mut app, SchemesMsg::RefreshGallery).units(), 0);
    finish_fetch(&mut app);
    assert_eq!(
        calls.lock().fetched,
        vec![(CATALOG_URL.to_owned(), MAX_CATALOG_BYTES)]
    );
    // Loaded: showing it again keeps the catalog; Refresh fetches again.
    assert_eq!(send(&mut app, SchemesMsg::ShowGallery(true)).units(), 0);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loaded(_)));
    assert_eq!(send(&mut app, SchemesMsg::RefreshGallery).units(), 1);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loading));
    finish_fetch(&mut app);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loaded(_)));
    // Closed and opened again in the same run: the catalog is still there, unfetched.
    let _ = send(&mut app, SchemesMsg::Close);
    let id = app.schemes_window();
    if let Some(id) = id {
        let _ = app.update(Message::WindowClosed(id));
    }
    let _ = send(&mut app, SchemesMsg::Open);
    assert_eq!(send(&mut app, SchemesMsg::ShowGallery(true)).units(), 0);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loaded(_)));
}

#[test]
fn nothing_fetches_without_the_gallery() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let _ = fake_io(&mut app, Ok(catalog(&[entry("x", 1, "#101010", false)])));
    let _ = send(&mut app, SchemesMsg::Open);
    let _ = send(&mut app, SchemesMsg::Select("harbor".into()));
    // No fetch was started: one would have left the gallery Loading.
    assert!(matches!(win(&app).gallery, GalleryLoad::Idle));
    assert!(!win(&app).showing_gallery);
    // Install and Update with no catalog do nothing.
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert!(scheme_files(&app).is_empty());
}

#[test]
fn bad_catalog_shows_error_and_keeps_installed() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let _ = open_gallery(&mut app, catalog(&[entry("x", 1, "#101010", false)]));
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    let before = read_file(&file(&app, "gallery-x")).unwrap();
    refresh_with(&mut app, b"<html>Not Found</html>".to_vec());
    assert!(
        failed(&app).starts_with("Couldn't load the theme gallery: "),
        "{}",
        failed(&app)
    );
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), before);
    assert_eq!(app.schemes.get("gallery-x"), Some(&before));
    // A catalog with only bad themes is the same line, not a crash.
    refresh_with(&mut app, catalog(&[entry("Bad Id", 1, "#101010", false)]));
    assert!(matches!(win(&app).gallery, GalleryLoad::Loaded(ref c) if c.themes.is_empty()));
}

#[test]
fn offline_shows_error_line() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let _ = fake_io(&mut app, Err("connection refused".into()));
    let _ = send(&mut app, SchemesMsg::Open);
    let _ = send(&mut app, SchemesMsg::ShowGallery(true));
    finish_fetch(&mut app);
    assert_eq!(
        failed(&app),
        "Couldn't load the theme gallery: connection refused"
    );
    // Retry is Refresh: it fetches again.
    let _ = fake_io(&mut app, Ok(catalog(&[entry("x", 1, "#101010", false)])));
    assert_eq!(send(&mut app, SchemesMsg::RefreshGallery).units(), 1);
    finish_fetch(&mut app);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loaded(_)));
}

#[test]
fn install_writes_gallery_file() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let bytes = catalog(&[entry("x", 2, "#101010", false)]);
    let _ = open_gallery(&mut app, bytes.clone());
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    let path = file(&app, "gallery-x");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[credit]"), "{text}");
    assert!(text.contains("[gallery]"), "{text}");
    let saved = read_file(&path).unwrap();
    assert_eq!(saved, installed_form(&bytes));
    let g = saved.gallery.as_ref().unwrap();
    assert_eq!((g.id.as_str(), g.version), ("x", 2));
    // A normal user scheme now: in the set and selectable in the window.
    assert_eq!(app.schemes.get("gallery-x"), Some(&saved));
    assert!(win(&app).error.is_none());
    assert_eq!(win(&app).notice.as_deref(), Some("Installed \"Theme x\""));
    let _ = send(&mut app, SchemesMsg::Select("gallery-x".into()));
    assert_eq!(win(&app).selected, "gallery-x");
    // Installing again at the same version is not offered: nothing changes.
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    assert_eq!(read_file(&path).unwrap(), saved);
    assert!(win(&app).pending_update.is_none());
    // An id the catalog does not have does nothing.
    let _ = send(&mut app, SchemesMsg::Install("nope".into()));
    assert_eq!(scheme_files(&app), vec!["gallery-x.toml".to_owned()]);
}

#[test]
fn update_in_place_recolors_open_panes() {
    let v1 = catalog(&[entry("x", 1, "#101010", false)]);
    let installed = installed_form(&v1);
    let a = with_scheme(&profile("a"), "gallery-x");
    let (mut app, _, wid, ids) = app_with(&[a], "", std::slice::from_ref(&installed));
    assert_eq!(style(&app, wid, ids[0]).scheme.background, rgb(0x101010));
    let _ = open_gallery(&mut app, catalog(&[entry("x", 2, "#203040", false)]));
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert!(win(&app).pending_update.is_none(), "unedited: no question");
    let saved = read_file(&file(&app, "gallery-x")).unwrap();
    assert_eq!(saved.colors.background, rgb(0x203040));
    assert_eq!(saved.gallery.as_ref().unwrap().version, 2);
    assert_eq!(style(&app, wid, ids[0]).scheme.background, rgb(0x203040));
    let t = crate::schemes_tests::term(&app, wid, ids[0]);
    assert!(crate::schemes_tests::answers(&t, rgb(0x203040)));
    assert_eq!(win(&app).notice.as_deref(), Some("Updated \"Theme x\""));
}

/// An installed v1 of `x` whose background the user changed to `#555555`.
fn edited_v1() -> Scheme {
    let mut s = installed_form(&catalog(&[entry("x", 1, "#101010", false)]));
    s.colors.background = rgb(0x555555);
    s
}

#[test]
fn update_of_edited_asks() {
    let v2 = catalog(&[entry("x", 2, "#203040", false)]);
    // Replace overwrites the edits.
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[edited_v1()]);
    let _ = open_gallery(&mut app, v2.clone());
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), edited_v1());
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Replace));
    assert!(win(&app).pending_update.is_none());
    assert_eq!(
        read_file(&file(&app, "gallery-x")).unwrap(),
        installed_form(&v2)
    );

    // Copy makes a new user scheme with the credit and no gallery block; the edited one stays.
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[edited_v1()]);
    let _ = open_gallery(&mut app, v2.clone());
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Copy));
    assert!(win(&app).pending_update.is_none());
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), edited_v1());
    let copies: Vec<&Scheme> = app
        .schemes
        .all()
        .iter()
        .filter(|s| s.id.starts_with("user-"))
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    let copy = copies[0];
    assert_eq!(copy.colors, installed_form(&v2).colors);
    assert_eq!(copy.credit, installed_form(&v2).credit);
    assert!(copy.credit.is_some());
    assert!(copy.gallery.is_none());
    assert!(file(&app, &copy.id).exists());
    let text = std::fs::read_to_string(file(&app, &copy.id)).unwrap();
    assert!(
        text.contains("[credit]") && !text.contains("[gallery]"),
        "{text}"
    );

    // Cancel leaves everything as it was.
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[edited_v1()]);
    let _ = open_gallery(&mut app, v2);
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Cancel));
    assert!(win(&app).pending_update.is_none());
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), edited_v1());
    assert_eq!(scheme_files(&app), vec!["gallery-x.toml".to_owned()]);
    // A choice with no question pending does nothing.
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Replace));
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), edited_v1());
}

#[test]
fn update_of_blockless_file_asks() {
    // A `gallery-x` scheme without a `[gallery]` block (an older fbterm saved it), same version.
    let bytes = catalog(&[entry("x", 1, "#101010", false)]);
    let blockless = Scheme {
        gallery: None,
        ..installed_form(&bytes)
    };
    let (mut app, _, _, _) = app_with(&[profile("a")], "", std::slice::from_ref(&blockless));
    let _ = open_gallery(&mut app, bytes.clone());
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), blockless);
    // Install asks the same question.
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Cancel));
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Replace));
    assert_eq!(
        read_file(&file(&app, "gallery-x")).unwrap(),
        installed_form(&bytes)
    );

    // A `gallery-x.toml` that does not load is not overwritten without asking either.
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    std::fs::create_dir_all(app.paths.schemes()).unwrap();
    std::fs::write(file(&app, "gallery-x"), "not a scheme").unwrap();
    let _ = app.reload_schemes();
    let _ = open_gallery(&mut app, bytes);
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    assert_eq!(
        std::fs::read_to_string(file(&app, "gallery-x")).unwrap(),
        "not a scheme"
    );
}

#[test]
fn withdrawn_installed_stays() {
    let v1 = catalog(&[entry("x", 1, "#101010", false)]);
    let installed = installed_form(&v1);
    let (mut app, _, _, _) = app_with(&[profile("a")], "", std::slice::from_ref(&installed));
    let _ = open_gallery(
        &mut app,
        catalog(&[
            entry("x", 2, "#203040", true),
            entry("y", 1, "#101010", true),
        ]),
    );
    for id in ["x", "y"] {
        let _ = send(&mut app, SchemesMsg::Update(id.into()));
        let _ = send(&mut app, SchemesMsg::Install(id.into()));
    }
    assert!(win(&app).pending_update.is_none());
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), installed);
    assert_eq!(app.schemes.get("gallery-x"), Some(&installed));
    assert!(!file(&app, "gallery-y").exists(), "withdrawn: not offered");
}

#[test]
fn newer_format_message() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let mut newer = catalog(&[entry("x", 1, "#101010", false)]);
    newer.splice(0..10, b"format = 2".iter().copied());
    let _ = fake_io(&mut app, Ok(newer));
    let _ = send(&mut app, SchemesMsg::Open);
    let _ = send(&mut app, SchemesMsg::ShowGallery(true));
    finish_fetch(&mut app);
    assert_eq!(failed(&app), "This catalog needs a newer fbterm.");
}

#[test]
fn credit_link_only_https() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let calls = fake_io(&mut app, Err("unused".into()));
    let _ = send(&mut app, SchemesMsg::Open);
    for bad in [
        "http://example.org/a",
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://",
        "https:///path",
        "https://example.org/a b",
        "https://example.org/\u{7}",
        "--help",
        "",
    ] {
        let _ = send(&mut app, SchemesMsg::OpenCreditLink(bad.into()));
    }
    assert!(calls.lock().opened.is_empty(), "{:?}", calls.lock().opened);
    let _ = send(
        &mut app,
        SchemesMsg::OpenCreditLink("https://example.org/x".into()),
    );
    assert_eq!(
        calls.lock().opened,
        vec!["https://example.org/x".to_owned()]
    );
}

fn close_window(app: &mut App) {
    let id = app.schemes_window().expect("schemes window open");
    let _ = app.update(Message::WindowClosed(id));
    assert!(app.schemes_win.is_none());
}

fn theme_names(app: &App) -> Vec<String> {
    match &win(app).gallery {
        GalleryLoad::Loaded(c) => c.themes.iter().map(|t| t.id.clone()).collect(),
        other => panic!("not loaded: {other:?}"),
    }
}

#[test]
fn reopen_during_loading_and_stale_results() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let _ = fake_io(&mut app, Ok(catalog(&[entry("a", 1, "#101010", false)])));
    let _ = send(&mut app, SchemesMsg::Open);
    assert_eq!(send(&mut app, SchemesMsg::ShowGallery(true)).units(), 1);
    let first = app.gallery_generation;
    // Closed and opened again while the fetch runs: still Loading, no second fetch.
    close_window(&mut app);
    let _ = send(&mut app, SchemesMsg::Open);
    assert!(matches!(win(&app).gallery, GalleryLoad::Loading));
    assert_eq!(send(&mut app, SchemesMsg::ShowGallery(true)).units(), 0);
    assert_eq!(send(&mut app, SchemesMsg::RefreshGallery).units(), 0);
    assert_eq!(app.gallery_generation, first);
    finish_fetch(&mut app);
    assert_eq!(theme_names(&app), ["a"]);

    // A newer fetch: an older result arriving meanwhile or afterwards changes nothing.
    let _ = fake_io(&mut app, Ok(catalog(&[entry("b", 1, "#101010", false)])));
    assert_eq!(send(&mut app, SchemesMsg::RefreshGallery).units(), 1);
    let stale = || Ok(catalog(&[entry("old", 1, "#101010", false)]));
    let _ = send(&mut app, SchemesMsg::GalleryFetched(first, stale()));
    assert!(matches!(win(&app).gallery, GalleryLoad::Loading));
    finish_fetch(&mut app);
    assert_eq!(theme_names(&app), ["b"]);
    let _ = send(&mut app, SchemesMsg::GalleryFetched(first, stale()));
    let _ = send(
        &mut app,
        SchemesMsg::GalleryFetched(first, Err("late".into())),
    );
    assert_eq!(theme_names(&app), ["b"]);
    let cached = app.gallery_catalog.as_ref().unwrap();
    assert_eq!(cached.themes[0].id, "b");
}

#[test]
fn refresh_clears_pending_update() {
    let v2 = catalog(&[entry("x", 2, "#203040", false)]);
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[edited_v1()]);
    let _ = open_gallery(&mut app, v2.clone());
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    let _ = fake_io(&mut app, Ok(v2));
    let _ = send(&mut app, SchemesMsg::RefreshGallery);
    assert!(win(&app).pending_update.is_none());
    finish_fetch(&mut app);
    // A late answer to the dropped question does nothing.
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Replace));
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), edited_v1());
}

#[test]
fn unsaved_draft_of_gallery_scheme_asks() {
    let v1 = catalog(&[entry("x", 1, "#101010", false)]);
    let v2 = catalog(&[entry("x", 2, "#203040", false)]);
    let installed = installed_form(&v1);
    let (mut app, _, _, _) = app_with(&[profile("a")], "", std::slice::from_ref(&installed));
    let _ = open_gallery(&mut app, v2.clone());
    let _ = send(&mut app, SchemesMsg::Select("gallery-x".into()));
    let _ = send(
        &mut app,
        SchemesMsg::Color(Entry::Background, rgb(0x555555)),
    );
    assert!(win(&app).is_dirty("gallery-x"));
    // An earlier error line goes when the question shows.
    let _ = send(&mut app, SchemesMsg::Rename(String::new()));
    let _ = send(&mut app, SchemesMsg::Save);
    assert!(win(&app).error.is_some());
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    assert!(win(&app).error.is_none());
    assert!(win(&app).notice.is_none());
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), installed);
    // Install asks the same way.
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Cancel));
    assert!(win(&app).is_dirty("gallery-x"), "Cancel keeps the draft");
    let _ = send(&mut app, SchemesMsg::Install("x".into()));
    assert_eq!(win(&app).pending_update.as_deref(), Some("x"));
    // Replace discards the draft: the window shows the new version, nothing unsaved.
    let _ = send(&mut app, SchemesMsg::UpdateChoice(UpdateChoice::Replace));
    let new = installed_form(&v2);
    assert_eq!(read_file(&file(&app, "gallery-x")).unwrap(), new);
    assert!(!win(&app).is_dirty("gallery-x"));
    assert_eq!(win(&app).draft("gallery-x"), Some(&new));
    assert_eq!(win(&app).hex_text(Entry::Background), "#203040");
    assert!(!win(&app).renaming);
}

/// A catalog entry with its own name, variant, credit project and author.
fn named(id: &str, name: &str, dark: bool, project: &str, author: &str) -> String {
    entry(id, 1, "#101010", false)
        .replace(
            &format!("name = \"Theme {id}\""),
            &format!("name = \"{name}\""),
        )
        .replace(
            "variant = \"dark\"",
            if dark {
                "variant = \"dark\""
            } else {
                "variant = \"light\""
            },
        )
        .replace(
            &format!("project = \"Project {id}\""),
            &format!("project = \"{project}\""),
        )
        .replace("author = \"Someone\"", &format!("author = \"{author}\""))
}

fn row_ids(rows: &[GalleryRow]) -> Vec<&str> {
    rows.iter().map(|r| r.id.as_str()).collect()
}

/// Four themes: two dark, two light, with distinct names, projects and authors.
fn four() -> Catalog {
    parse_catalog(&catalog(&[
        named("pine", "pine Night", true, "Forest Colors", "Ada Grey"),
        named("dune", "Dune", false, "Desert Set", "Bo Lane"),
        named("ash", "Ash", true, "Embers", "Cy Moss"),
        named("oat", "oat Milk", false, "Breakfast", "Dee Park"),
    ]))
    .unwrap()
}

#[test]
fn gallery_rows_sorted_by_name_ignoring_case() {
    let (app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let rows = gallery_rows(&four(), &app.schemes, "", None);
    assert_eq!(row_ids(&rows), ["ash", "dune", "oat", "pine"]);
    let ash = &rows[0];
    assert_eq!(ash.name, "Ash");
    assert_eq!(ash.credit_line, "Embers by Cy Moss, MIT");
    assert_eq!(ash.link.as_deref(), Some("https://example.org/ash"));
    assert!(ash.dark);
    assert_eq!(ash.button, Some(RowButton::Install));
    assert_eq!(ash.note, None);
    assert_eq!(ash.colors.background, rgb(0x101010));
}

#[test]
fn gallery_filter_matches_name_project_and_author() {
    let (app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let cat = four();
    let ids = |filter: &str, variant| {
        row_ids(&gallery_rows(&cat, &app.schemes, filter, variant))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // Name, case-insensitive, trimmed.
    assert_eq!(ids("  NIGHT ", None), ["pine"]);
    // Credit project.
    assert_eq!(ids("desert", None), ["dune"]);
    // Credit author.
    assert_eq!(ids("dee p", None), ["oat"]);
    // Anything else matches nothing.
    assert!(ids("zzz", None).is_empty());
    assert!(ids("MIT", None).is_empty(), "the license is not searched");
    // Dark or light, with and without text.
    assert_eq!(ids("", Some(true)), ["ash", "pine"]);
    assert_eq!(ids("", Some(false)), ["dune", "oat"]);
    assert_eq!(ids("embers", Some(false)), Vec::<String>::new());
}

#[test]
fn gallery_row_buttons_follow_install_state() {
    let v1 = catalog(&[entry("x", 1, "#101010", false)]);
    let old = installed_form(&v1);
    let same = installed_form(&catalog(&[entry("y", 1, "#101010", false)]));
    let gone = installed_form(&catalog(&[entry("w", 1, "#101010", false)]));
    let (app, _, _, _) = app_with(&[profile("a")], "", &[old, same, gone]);
    let cat = parse_catalog(&catalog(&[
        entry("x", 2, "#203040", false),
        entry("y", 1, "#101010", false),
        entry("z", 1, "#101010", false),
        entry("w", 3, "#101010", true),
        entry("v", 1, "#101010", true),
    ]))
    .unwrap();
    let rows = gallery_rows(&cat, &app.schemes, "", None);
    let by = |id: &str| rows.iter().find(|r| r.id == id);
    assert_eq!(by("x").unwrap().button, Some(RowButton::Update));
    assert_eq!(by("y").unwrap().button, Some(RowButton::Installed));
    assert_eq!(by("z").unwrap().button, Some(RowButton::Install));
    let w = by("w").unwrap();
    assert_eq!(w.button, None);
    assert_eq!(w.note, Some(NO_LONGER_OFFERED));
    assert_eq!(NO_LONGER_OFFERED, "No longer offered");
    assert!(by("v").is_none(), "withdrawn and not installed: hidden");
    assert!(rows.iter().all(|r| r.id == "w" || r.note.is_none()));
}

#[test]
fn credit_line_and_scheme_credit() {
    let v1 = catalog(&[entry("x", 1, "#101010", false)]);
    let installed = installed_form(&v1);
    let credit = installed.credit.clone().unwrap();
    assert_eq!(credit_line(&credit), "Project x by Someone, MIT");
    // A gallery id without its credit block takes the loaded catalog's credit.
    let bare = Scheme {
        credit: None,
        gallery: None,
        ..installed.clone()
    };
    let (mut app, _, _, _) = app_with(&[profile("a")], "", std::slice::from_ref(&bare));
    let _ = send(&mut app, SchemesMsg::Open);
    assert_eq!(win(&app).credit_of(&bare), None, "no catalog loaded: none");
    let _ = open_gallery(&mut app, v1);
    assert_eq!(win(&app).credit_of(&bare), Some(&credit));
    assert_eq!(win(&app).credit_of(&installed), Some(&credit));
}

#[test]
fn gallery_search_and_variant_live_in_the_window() {
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[]);
    let _ = send(&mut app, SchemesMsg::Open);
    // Filters work on a read-only (built-in) selection too, and keep the last notice.
    win_mut(&mut app).notice = Some("Installed \"X\"".into());
    let _ = send(&mut app, SchemesMsg::GalleryQuery("dune".into()));
    let _ = send(&mut app, SchemesMsg::GalleryVariant(Some(false)));
    assert_eq!(win(&app).gallery_query, "dune");
    assert_eq!(win(&app).gallery_variant, Some(false));
    assert!(win(&app).notice.is_some());
    // Picking a scheme in the list leaves the gallery for the editor.
    let _ = send(&mut app, SchemesMsg::ShowGallery(true));
    assert!(win(&app).showing_gallery);
    let first = win(&app).drafts()[1].id.clone();
    let _ = send(&mut app, SchemesMsg::Select(first.clone()));
    assert!(!win(&app).showing_gallery);
    assert_eq!(win(&app).selected, first);
}

fn win_mut(app: &mut App) -> &mut SchemesState {
    &mut app.schemes_win.as_mut().expect("schemes window open").1
}

#[test]
fn edited_update_question_names_the_theme() {
    let v2 = catalog(&[entry("x", 2, "#203040", false)]);
    let (mut app, _, _, _) = app_with(&[profile("a")], "", &[edited_v1()]);
    let _ = open_gallery(&mut app, v2);
    assert_eq!(win(&app).update_question(), None);
    let _ = send(&mut app, SchemesMsg::Update("x".into()));
    assert_eq!(
        win(&app).update_question(),
        Some(("Update \"Theme x\"?".to_owned(), EDITED_HERE))
    );
    assert_eq!(EDITED_HERE, "You changed this theme on this machine.");
}

#[test]
fn demo_gallery_shows_every_row_kind() {
    let (catalog, installed) = super::gallery::demo_catalog();
    assert!(catalog.skipped.is_empty(), "{:?}", catalog.skipped);
    let (app, _, _, _) = app_with(&[profile("a")], "", &installed);
    let rows = gallery_rows(&catalog, &app.schemes, "", None);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].button, Some(RowButton::Install));
    let kinds: Vec<_> = rows
        .iter()
        .map(|r| (r.name.as_str(), r.button, r.note))
        .collect();
    let long = rows[0].name.as_str();
    assert!(long.starts_with("A Very Long Sample Theme Name"));
    assert!(long.chars().count() >= 120, "worst-case name: {long}");
    assert!(rows[0].credit_line.chars().count() >= 400);
    assert_eq!(
        kinds[1..],
        [
            ("Dusk", Some(RowButton::Install), None),
            ("Lagoon", Some(RowButton::Installed), None),
            ("Meadow", Some(RowButton::Update), None),
            ("Slate", None, Some(NO_LONGER_OFFERED)),
        ]
    );
    assert!(rows.iter().any(|r| !r.dark));
    // Readable sample themes, so a screenshot shows no contrast hint.
    for t in &catalog.themes {
        assert!(
            fbterm_config::schemes::contrast_ok(&t.scheme.colors),
            "{} misses the contrast bar",
            t.id
        );
    }
}

#[test]
fn clip_chars_cuts_with_an_ellipsis() {
    assert_eq!(clip_chars("Dusk", 10), "Dusk");
    assert_eq!(clip_chars("abcdefghij", 10), "abcdefghij");
    assert_eq!(clip_chars("abcdefghijk", 10), "abcdefghi\u{2026}");
    // Characters, not bytes, and no trailing space before the ellipsis.
    assert_eq!(
        clip_chars("\u{e9}\u{e9}\u{e9} \u{e9}\u{e9}", 5),
        "\u{e9}\u{e9}\u{e9}\u{2026}"
    );
}
