use super::*;

fn state() -> SettingsState {
    SettingsState::new(
        &Settings::default(),
        vec!["Menlo".into(), "Courier New".into(), "Geist Mono".into()],
        None,
    )
}

#[test]
fn a_click_applies_and_writes_at_once() {
    let mut s = state();
    let e = s.update(SettingsMsg::Theme(ThemeMode::Light));
    assert_eq!(e.apply, Some(SettingChange::Theme(ThemeMode::Light)));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.values.theme, ThemeMode::Light);
    assert_eq!(s.take_pending(), [SettingChange::Theme(ThemeMode::Light)]);
    // Choosing what is already chosen changes nothing.
    let e = s.update(SettingsMsg::Theme(ThemeMode::Light));
    assert_eq!(e, Effect::NONE);
}

#[test]
fn a_drag_applies_each_step_and_writes_once_it_settles() {
    let mut s = state();
    let mut last = 0;
    for v in [0.9, 0.8, 0.7] {
        let e = s.update(SettingsMsg::Opacity(v));
        assert!(e.apply.is_some());
        let Write::After(n) = e.write else {
            panic!("a drag waits before writing: {e:?}");
        };
        assert!(n > last);
        last = n;
    }
    // Only the last change's timer writes, and only the latest value.
    assert_eq!(s.update(SettingsMsg::Flush(last - 1)).write, Write::Not);
    assert_eq!(s.update(SettingsMsg::Flush(last)).write, Write::Now);
    assert_eq!(s.take_pending(), [SettingChange::BackgroundOpacity(0.7)]);
    assert_eq!(s.opacity_text, "70");
    // Nothing pending: a late timer or a release writes nothing.
    assert_eq!(s.update(SettingsMsg::Flush(last)).write, Write::Not);
    assert_eq!(s.update(SettingsMsg::Release).write, Write::Not);
}

#[test]
fn letting_go_of_a_slider_writes_now() {
    let mut s = state();
    s.update(SettingsMsg::FontSize(15.2));
    assert_eq!(s.values.font_size, 15.0, "the slider moves in half points");
    assert_eq!(s.update(SettingsMsg::Release).write, Write::Now);
}

#[test]
fn sliders_clamp_to_the_allowed_ranges() {
    let mut s = state();
    s.update(SettingsMsg::Opacity(0.0));
    assert_eq!(s.values.background_opacity, OPACITY_RANGE.0);
    s.update(SettingsMsg::FontSize(500.0));
    assert_eq!(s.values.font_size, FONT_SIZE_RANGE.1);
    assert_eq!(s.font_size_text, "72");
}

#[test]
fn typed_numbers_apply_only_in_range_and_clamp_on_enter() {
    let mut s = state();
    // "1" on the way to "14" is out of range: nothing applies yet.
    assert_eq!(
        s.update(SettingsMsg::FontSizeText("1".into())),
        Effect::NONE
    );
    assert_eq!(s.values.font_size, 13.0);
    let e = s.update(SettingsMsg::FontSizeText("14".into()));
    assert_eq!(e.apply, Some(SettingChange::FontSize(14.0)));
    assert!(matches!(e.write, Write::After(_)));
    s.update(SettingsMsg::FontSizeText("200".into()));
    assert_eq!(s.values.font_size, 14.0);
    let e = s.update(SettingsMsg::FontSizeSubmit);
    assert_eq!(e.apply, Some(SettingChange::FontSize(72.0)));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.font_size_text, "72");
    // Garbage goes back to the current value.
    s.update(SettingsMsg::FontSizeText("big".into()));
    s.update(SettingsMsg::FontSizeSubmit);
    assert_eq!(s.font_size_text, "72");
}

#[test]
fn opacity_is_typed_in_percent() {
    let mut s = state();
    let e = s.update(SettingsMsg::OpacityText("85%".into()));
    assert_eq!(e.apply, Some(SettingChange::BackgroundOpacity(0.85)));
    s.update(SettingsMsg::OpacityText("2".into()));
    assert_eq!(
        s.values.background_opacity, 0.85,
        "below 5% waits for Enter"
    );
    s.update(SettingsMsg::OpacitySubmit);
    assert_eq!(s.values.background_opacity, OPACITY_RANGE.0);
    assert_eq!(s.opacity_text, "5");
}

#[test]
fn scrollback_takes_digits_and_clamps() {
    let mut s = state();
    let e = s.update(SettingsMsg::ScrollbackText("20,000".into()));
    assert_eq!(e.apply, Some(SettingChange::ScrollbackLines(20_000)));
    s.update(SettingsMsg::ScrollbackText("99999999".into()));
    assert_eq!(s.values.scrollback_lines, 20_000);
    s.update(SettingsMsg::ScrollbackSubmit);
    assert_eq!(s.values.scrollback_lines, SCROLLBACK_RANGE.1);
    assert_eq!(s.scrollback_text, "1000000");
}

#[test]
fn a_read_only_window_changes_nothing() {
    let err = ConfigError {
        path: "settings.toml".into(),
        line: Some(2),
        message: "invalid".into(),
    };
    let mut s = SettingsState::new(&Settings::default(), Vec::new(), Some(err));
    assert_eq!(s.update(SettingsMsg::Kitty(true)), Effect::NONE);
    assert_eq!(s.update(SettingsMsg::FontSize(20.0)), Effect::NONE);
    assert!(!s.values.kitty_keyboard);
    assert!(!s.has_pending());
}

#[test]
fn an_edit_on_disk_resets_the_controls_and_drops_pending_writes() {
    let mut s = state();
    s.update(SettingsMsg::Opacity(0.5));
    let disk = Settings {
        font_family: "Iosevka".into(),
        font_size: 18.0,
        background_opacity: 0.9,
        ..Settings::default()
    };
    s.sync(&disk);
    assert!(!s.has_pending());
    assert_eq!(s.values, disk);
    assert_eq!(s.font_size_text, "18");
    assert_eq!(s.opacity_text, "90");
    let chosen = s.selected_font().unwrap();
    assert_eq!(chosen.to_string(), "Iosevka (not found)");
}

#[test]
fn the_font_list_puts_the_bundled_face_first() {
    let list = fonts::choices(
        vec!["menlo".into(), "Courier New".into(), "Geist Mono".into()],
        "Geist Mono",
    );
    let names: Vec<String> = list.iter().map(ToString::to_string).collect();
    assert_eq!(names, ["Geist Mono", "Courier New", "menlo"]);
    let list = fonts::choices(vec!["Menlo".into()], "Missing Mono");
    let names: Vec<String> = list.iter().map(ToString::to_string).collect();
    assert_eq!(names, ["Geist Mono", "Missing Mono (not found)", "Menlo"]);
    let list = fonts::choices(vec!["Menlo".into()], "Menlo");
    assert!(list.iter().all(|f| f.installed));
}

#[test]
fn typed_opacity_is_kept_in_whole_percent() {
    let mut s = state();
    let e = s.update(SettingsMsg::OpacityText("85.4".into()));
    assert_eq!(e.apply, Some(SettingChange::BackgroundOpacity(0.85)));
    s.update(SettingsMsg::OpacityText("85.5".into()));
    s.update(SettingsMsg::OpacitySubmit);
    assert_eq!(s.values.background_opacity, 0.86);
    assert_eq!(s.opacity_text, "86");
}

#[test]
fn unwritten_changes_go_back_in_line_unless_newer_ones_wait() {
    let mut s = state();
    s.update(SettingsMsg::Kitty(true));
    let failed = s.take_pending();
    s.update(SettingsMsg::Theme(ThemeMode::Light));
    s.requeue(failed);
    assert_eq!(
        s.take_pending(),
        [
            SettingChange::KittyKeyboard(true),
            SettingChange::Theme(ThemeMode::Light)
        ]
    );
    s.update(SettingsMsg::Kitty(false));
    s.requeue(vec![SettingChange::KittyKeyboard(true)]);
    assert_eq!(s.take_pending(), [SettingChange::KittyKeyboard(false)]);
}

#[test]
fn blur_says_when_it_has_no_effect() {
    assert_eq!(
        view::blur_hint(true, 0.8),
        ("Applies to new windows", false)
    );
    assert_eq!(
        view::blur_hint(true, 1.0),
        ("Needs opacity below 100%", true)
    );
    assert_eq!(view::blur_hint(false, 0.8), ("macOS only for now", true));
}
