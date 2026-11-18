use super::*;
use fbterm_config::keys::{Action, Chord};

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
    let e = s.update(SettingsMsg::Kitty(true));
    assert_eq!(e.apply, Some(SettingChange::KittyKeyboard(true)));
    assert_eq!(e.write, Write::Now);
    assert!(s.values.kitty_keyboard);
    assert_eq!(s.take_pending(), [SettingChange::KittyKeyboard(true)]);
    // Choosing what is already chosen changes nothing.
    let e = s.update(SettingsMsg::Kitty(true));
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
fn ligatures_switch_queues_its_setting() {
    let mut s = state();
    s.update(SettingsMsg::Ligatures(false));
    assert_eq!(s.take_pending(), [SettingChange::Ligatures(false)]);
}

#[test]
fn clusters_switch_queues_its_setting() {
    let mut s = state();
    s.update(SettingsMsg::Clusters(true));
    assert_eq!(s.take_pending(), [SettingChange::GraphemeClusters(true)]);
}

#[test]
fn inline_images_switch_queues_its_setting() {
    let mut s = state();
    assert!(s.values.inline_images, "on by default");
    let e = s.update(SettingsMsg::InlineImages(false));
    assert_eq!(e.apply, Some(SettingChange::InlineImages(false)));
    assert_eq!(e.write, Write::Now);
    assert!(!s.values.inline_images);
    assert_eq!(s.take_pending(), [SettingChange::InlineImages(false)]);
}

/// The `Images in scrollback` switch belongs to the `Inline images` one: usable while pictures
/// are shown, muted and inert without them.
#[test]
fn scrollback_images_switch_needs_inline_images() {
    let mut s = state();
    assert!(
        s.scrollback_images_enabled(),
        "inline images is on by default"
    );
    let e = s.update(SettingsMsg::ScrollbackImages(true));
    assert_eq!(e.apply, Some(SettingChange::ScrollbackImages(true)));
    assert_eq!(s.take_pending(), [SettingChange::ScrollbackImages(true)]);
    // Inline images off: the switch is disabled and its message changes nothing.
    s.update(SettingsMsg::InlineImages(false));
    s.take_pending();
    assert!(!s.scrollback_images_enabled());
    let values = s.values.clone();
    assert_eq!(s.update(SettingsMsg::ScrollbackImages(false)), Effect::NONE);
    assert_eq!(s.values, values);
    assert!(!s.has_pending());
    // Back on: the value it kept is usable again.
    s.update(SettingsMsg::InlineImages(true));
    assert!(s.scrollback_images_enabled());
    assert!(s.values.scrollback_images, "the setting was never lost");
}

/// The two rows as the tab draws them: their labels and hints, and what a click on each switch
/// publishes. A disabled row publishes nothing.
#[test]
fn the_image_rows_say_what_they_do_and_a_disabled_one_publishes_nothing() {
    use crate::app::Message;
    const INLINE: &str = "Show pictures that programs send";
    const SCROLLBACK: &str = "Keep pictures when they scroll off the screen";
    let t = crate::theme::graphite_tokens();
    // A switch sits at the row's right end: press it 16 px of padding plus half its 18 px width
    // in from the edge.
    let inset = 16.0 + 9.0;

    let (messages, texts) = press_right_end(view::inline_images_setting(t, true, true), inset);
    assert!(
        matches!(
            messages.as_slice(),
            [Message::Settings(SettingsMsg::InlineImages(false))]
        ),
        "{messages:?}"
    );
    for want in ["Inline images", INLINE] {
        assert!(texts.iter().any(|x| x == want), "{want}: {texts:?}");
    }

    let (messages, texts) =
        press_right_end(view::scrollback_images_setting(t, false, true, true), inset);
    assert!(
        matches!(
            messages.as_slice(),
            [Message::Settings(SettingsMsg::ScrollbackImages(true))]
        ),
        "{messages:?}"
    );
    for want in ["Images in scrollback", SCROLLBACK] {
        assert!(texts.iter().any(|x| x == want), "{want}: {texts:?}");
    }

    // Inline images off: the row is drawn, its switch publishes nothing.
    let (messages, texts) = press_right_end(
        view::scrollback_images_setting(t, false, false, true),
        inset,
    );
    assert!(messages.is_empty(), "{messages:?}");
    for want in ["Images in scrollback", SCROLLBACK] {
        assert!(texts.iter().any(|x| x == want), "{want}: {texts:?}");
    }
    // The file does not load: neither switch publishes.
    let (messages, _) = press_right_end(view::inline_images_setting(t, true, false), inset);
    assert!(messages.is_empty(), "{messages:?}");
    let (messages, _) = press_right_end(
        view::scrollback_images_setting(t, false, true, false),
        inset,
    );
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn unwritten_changes_go_back_in_line_unless_newer_ones_wait() {
    let mut s = state();
    s.update(SettingsMsg::Kitty(true));
    let failed = s.take_pending();
    s.update(SettingsMsg::Ligatures(false));
    s.requeue(failed);
    assert_eq!(
        s.take_pending(),
        [
            SettingChange::KittyKeyboard(true),
            SettingChange::Ligatures(false)
        ]
    );
    s.update(SettingsMsg::Kitty(false));
    s.requeue(vec![SettingChange::KittyKeyboard(true)]);
    assert_eq!(s.take_pending(), [SettingChange::KittyKeyboard(false)]);
}

#[test]
fn blur_says_when_it_has_no_effect() {
    let row = view::blur_row(true, 0.8);
    assert_eq!(
        (row.hint, row.inert, row.enabled),
        ("Applies to new windows", false, true)
    );
    let row = view::blur_row(true, 1.0);
    assert_eq!(
        (row.hint, row.inert, row.enabled),
        ("Needs opacity below 100%", true, true)
    );
}

#[test]
fn blur_switch_disabled_when_unsupported() {
    use crate::chrome::{BlurProbe, Platform, blur_supported};
    let probe = |windows_build: Option<u32>, wayland: bool, kde: bool| BlurProbe {
        windows_build,
        wayland,
        kde,
    };
    // macOS always blurs; Windows 11 (build 22621 and later) through its backdrop; Linux
    // where the compositor offers the KDE blur protocol (Wayland under KDE).
    assert!(blur_supported(Platform::MacOs, &probe(None, false, false)));
    assert!(blur_supported(
        Platform::Windows,
        &probe(Some(22631), false, false)
    ));
    assert!(!blur_supported(
        Platform::Windows,
        &probe(Some(19045), false, false)
    ));
    assert!(!blur_supported(
        Platform::Windows,
        &probe(None, false, false)
    ));
    assert!(blur_supported(Platform::Linux, &probe(None, true, true)));
    assert!(!blur_supported(Platform::Linux, &probe(None, false, true)));
    assert!(!blur_supported(Platform::Linux, &probe(None, true, false)));
    // Unsupported: the switch cannot be used and says why, whatever the opacity.
    for opacity in [0.8, 1.0] {
        let row = view::blur_row(false, opacity);
        assert_eq!(row.hint, "Not supported on this system");
        assert!(!row.enabled);
        assert!(row.inert);
    }
}

#[test]
fn reg_runs_from_system32_by_full_path() {
    use crate::chrome::reg_exe;
    assert_eq!(
        reg_exe(Some(r"D:\Win".into())),
        std::ffi::OsString::from(r"D:\Win\System32\reg.exe")
    );
    assert_eq!(
        reg_exe(Some(r"C:\Windows\".into())),
        std::ffi::OsString::from(r"C:\Windows\System32\reg.exe")
    );
    for unset in [None, Some("".into())] {
        assert_eq!(
            reg_exe(unset),
            std::ffi::OsString::from(r"C:\Windows\System32\reg.exe")
        );
    }
}

#[test]
fn the_windows_build_is_read_from_the_registry_output() {
    use crate::chrome::windows_build;
    let reg = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\r\n    \
               CurrentBuildNumber    REG_SZ    22631\r\n\r\n";
    assert_eq!(windows_build(reg), Some(22631));
    assert_eq!(
        windows_build("CurrentBuildNumber\tREG_SZ\t19045  \n"),
        Some(19045)
    );
    // A missing value, another value, a wrong type or a non-number: unknown.
    assert_eq!(windows_build(""), None);
    assert_eq!(
        windows_build(
            "ERROR: The system was unable to find the specified registry key or value.\r\n"
        ),
        None
    );
    assert_eq!(
        windows_build("    CurrentBuild    REG_SZ    22631\r\n"),
        None
    );
    assert_eq!(
        windows_build("    CurrentBuildNumber    REG_DWORD    0x5867\r\n"),
        None
    );
    assert_eq!(
        windows_build("    CurrentBuildNumber    REG_SZ    22631a\r\n"),
        None
    );
    assert_eq!(windows_build("    CurrentBuildNumber    REG_SZ\r\n"), None);
    assert_eq!(
        windows_build("    CurrentBuildNumber    REG_SZ    22631 extra\r\n"),
        None
    );
    // `ver` output (which is translated) is not read.
    assert_eq!(
        windows_build("Microsoft Windows [Version 10.0.22631.4317]"),
        None
    );
}

#[test]
fn an_unsupported_blur_switch_cannot_be_toggled() {
    use crate::app::Message;
    let t = crate::theme::graphite_tokens();
    // Lay the blur row out, click the switch at its right end: what does it publish and say?
    let run = |supported: bool| {
        press_right_end(
            view::blur_setting(t, false, true, supported, 0.8),
            16.0 + 9.0,
        )
    };
    let (messages, texts) = run(true);
    assert!(
        matches!(
            messages.as_slice(),
            [Message::Settings(SettingsMsg::Blur(true))]
        ),
        "{messages:?}"
    );
    assert!(
        texts.iter().any(|x| x == "Applies to new windows"),
        "{texts:?}"
    );
    let (messages, texts) = run(false);
    assert!(messages.is_empty(), "{messages:?}");
    assert!(
        texts.iter().any(|x| x == "Not supported on this system"),
        "{texts:?}"
    );
}

/// Lay `element` out at 560x72, press at its right end `inset` px in from the edge, and say
/// what it published and which texts it shows.
fn press_right_end(
    mut element: iced::Element<'_, crate::app::Message>,
    inset: f32,
) -> (Vec<crate::app::Message>, Vec<String>) {
    use iced::advanced::layout::{self, Layout};
    use iced::advanced::widget::{Operation, Tree};
    use iced::advanced::{Shell, clipboard};
    use iced::{Event, Pixels, Point, Rectangle, Size, mouse};

    struct Texts(Vec<String>);
    impl Operation for Texts {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _id: Option<&iced::advanced::widget::Id>, _b: Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }
    let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        Pixels(16.0),
    ));
    let size = Size::new(560.0, 72.0);
    let mut tree = Tree::new(&element);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    let bounds = node.bounds();
    let mut texts = Texts(Vec::new());
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut texts);
    let at = Point::new(bounds.width - inset, bounds.height / 2.0);
    let mut messages = Vec::new();
    // A press and its release: buttons publish on release, switches on press.
    for event in [
        mouse::Event::ButtonPressed(mouse::Button::Left),
        mouse::Event::ButtonReleased(mouse::Button::Left),
    ] {
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(event),
            Layout::new(&node),
            mouse::Cursor::Available(at),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(size),
        );
    }
    (messages, texts.0)
}

#[test]
fn the_blur_style_row_is_macos_only() {
    use crate::chrome::Platform;
    assert!(view::shows_blur_style_row(Platform::MacOs));
    assert!(!view::shows_blur_style_row(Platform::Windows));
    assert!(!view::shows_blur_style_row(Platform::Linux));
}

#[test]
fn the_blur_style_row_is_usable_only_while_blur_shows() {
    // Same rule as the blur switch's effect: blur on, below full opacity, on a system that
    // blurs.
    assert!(view::blur_style_enabled(true, true, 0.8));
    assert!(!view::blur_style_enabled(false, true, 0.8), "blur off");
    assert!(!view::blur_style_enabled(true, true, 1.0), "opacity 100%");
    assert!(!view::blur_style_enabled(true, false, 0.8), "unsupported");
}

#[test]
fn choosing_a_blur_style_applies_and_saves_it() {
    use fbterm_config::MacosBlurStyle;
    let mut s = state();
    let e = s.update(SettingsMsg::BlurStyle(MacosBlurStyle::Classic));
    assert_eq!(
        e.apply,
        Some(SettingChange::MacosBlurStyle(MacosBlurStyle::Classic))
    );
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.values.macos_blur_style, MacosBlurStyle::Classic);
    assert_eq!(
        s.take_pending(),
        [SettingChange::MacosBlurStyle(MacosBlurStyle::Classic)]
    );
    assert_eq!(
        s.update(SettingsMsg::BlurStyle(MacosBlurStyle::Classic)),
        Effect::NONE
    );
}

#[test]
fn the_blur_style_row_offers_both_styles_and_is_inert_without_blur() {
    use crate::app::Message;
    use fbterm_config::MacosBlurStyle;
    let t = crate::theme::graphite_tokens();
    const NOTE: &str = "Native uses the macOS look. Classic is fbterm's earlier, clearer blur.";
    // The right segment is `Classic`: press inside it, clear of the card's padding.
    let inset = 16.0 + 2.0 + 12.0;
    let (messages, texts) = press_right_end(
        view::blur_style_setting(t, &MacosBlurStyle::Native, true, true),
        inset,
    );
    assert!(
        matches!(
            messages.as_slice(),
            [Message::Settings(SettingsMsg::BlurStyle(
                MacosBlurStyle::Classic
            ))]
        ),
        "{messages:?}"
    );
    for want in ["Blur style", "Native", "Classic", NOTE] {
        assert!(texts.iter().any(|x| x == want), "{want}: {texts:?}");
    }
    // Disabled (blur off or opaque): nothing to press.
    let (messages, texts) = press_right_end(
        view::blur_style_setting(t, &MacosBlurStyle::Native, true, false),
        inset,
    );
    assert!(messages.is_empty(), "{messages:?}");
    assert!(texts.iter().any(|x| x == NOTE), "{texts:?}");
    // Not editable (the file does not load): nothing to press either.
    let (messages, _) = press_right_end(
        view::blur_style_setting(t, &MacosBlurStyle::Native, false, true),
        inset,
    );
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn the_title_bar_row_is_linux_only_and_writes_the_setting() {
    use crate::chrome::Platform;
    use fbterm_config::WindowDecorations;
    assert!(view::shows_title_bar_row(Platform::Linux));
    assert!(!view::shows_title_bar_row(Platform::Windows));
    assert!(!view::shows_title_bar_row(Platform::MacOs));
    let mut s = state();
    s.update(SettingsMsg::TitleBar(WindowDecorations::System));
    assert_eq!(s.values.window_decorations, WindowDecorations::System);
    assert_eq!(
        s.take_pending(),
        [SettingChange::WindowDecorations(WindowDecorations::System)]
    );
}

#[test]
fn the_tabs_row_changes_the_side_at_once() {
    use fbterm_config::TabBar;
    let mut s = state();
    s.update(SettingsMsg::TabBar(TabBar::Left));
    assert_eq!(s.values.tab_bar, Some(TabBar::Left));
    assert_eq!(s.take_pending(), [SettingChange::TabBar(TabBar::Left)]);
}

#[test]
fn the_group_tabs_switch_changes_the_setting_at_once() {
    let mut s = state();
    s.update(SettingsMsg::TabGroups(true));
    assert!(s.values.tab_groups);
    assert_eq!(s.take_pending(), [SettingChange::TabGroups(true)]);
}

fn labels(s: &SettingsState) -> Vec<String> {
    s.rate_choices().into_iter().map(|c| c.label).collect()
}

#[test]
fn the_rate_list_offers_presets_and_the_display() {
    let mut s = state();
    assert_eq!(labels(&s), ["30", "60", "120", "Match display"]);
    assert_eq!(s.selected_rate().unwrap().value, MaxFps::Fixed(60));
    s.display_hz = Some(120);
    assert_eq!(labels(&s), ["30", "60", "120", "Match display (120 Hz)"]);
}

#[test]
fn a_custom_rate_from_the_file_is_listed_and_chosen() {
    let values = Settings {
        max_fps: MaxFps::Fixed(90),
        ..Settings::default()
    };
    let mut s = SettingsState::new(&values, Vec::new(), None);
    assert_eq!(
        labels(&s),
        ["30", "60", "90 (custom)", "120", "Match display"]
    );
    assert_eq!(s.selected_rate().unwrap().label, "90 (custom)");
    s.sync(&Settings {
        max_fps: MaxFps::Fixed(240),
        ..Settings::default()
    });
    assert_eq!(labels(&s)[3], "240 (custom)");
}

#[test]
fn choosing_a_rate_applies_and_writes_at_once() {
    let mut s = state();
    let e = s.update(SettingsMsg::MaxFps(MaxFps::Display));
    assert_eq!(e.apply, Some(SettingChange::MaxFps(MaxFps::Display)));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.selected_rate().unwrap().value, MaxFps::Display);
    let e = s.update(SettingsMsg::MaxFps(MaxFps::Fixed(120)));
    assert_eq!(e.apply, Some(SettingChange::MaxFps(MaxFps::Fixed(120))));
}

#[test]
fn the_cpu_hint_shows_above_60() {
    let mut s = state();
    assert_eq!(s.rate_hint(), RATE_HINT);
    s.update(SettingsMsg::MaxFps(MaxFps::Fixed(120)));
    assert_eq!(s.rate_hint(), RATE_CPU_HINT);
    s.update(SettingsMsg::MaxFps(MaxFps::Fixed(30)));
    assert_eq!(s.rate_hint(), RATE_HINT);
    // Following the display: only when the display is faster than 60 Hz.
    s.update(SettingsMsg::MaxFps(MaxFps::Display));
    assert_eq!(s.rate_hint(), RATE_HINT, "unknown display: 60");
    s.display_hz = Some(60);
    assert_eq!(s.rate_hint(), RATE_HINT);
    s.display_hz = Some(120);
    assert_eq!(s.rate_hint(), RATE_CPU_HINT);
}

fn chord(s: &str) -> Chord {
    Chord::parse(s).unwrap().unwrap()
}

/// A chord no action has by default on any platform.
const FREE: &str = "ctrl+alt+shift+k";

#[test]
fn a_recorded_free_chord_is_applied_and_written_at_once() {
    let mut s = state();
    s.update(SettingsMsg::Record(Action::NewTab));
    assert!(s.recording.is_some());
    let e = s.update(SettingsMsg::Pressed(Some(chord(FREE))));
    assert!(s.recording.is_none());
    let Some(SettingChange::Keys(o)) = e.apply else {
        panic!("{e:?}");
    };
    assert_eq!(o.get(&Action::NewTab), Some(&Some(chord(FREE))));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.bindings().chord(Action::NewTab), Some(chord(FREE)));
    // Recording its own chord again changes nothing.
    s.update(SettingsMsg::Record(Action::NewTab));
    assert_eq!(
        s.update(SettingsMsg::Pressed(Some(chord(FREE)))),
        Effect::NONE
    );
    assert!(s.recording.is_none());
}

#[test]
fn a_press_the_system_kept_keeps_recording_and_says_so() {
    let mut s = state();
    // Not recording: nothing to say.
    assert_eq!(
        s.update(SettingsMsg::Withheld(Some(chord("ctrl+shift+0")))),
        Effect::NONE
    );
    assert!(s.recording.is_none());
    s.update(SettingsMsg::Record(Action::NewTab));
    assert_eq!(
        s.update(SettingsMsg::Withheld(Some(chord("ctrl+shift+0")))),
        Effect::NONE
    );
    let Some(RecordProblem::NotAllowed(why)) =
        &s.recording.as_ref().expect("still recording").problem
    else {
        panic!("{:?}", s.recording);
    };
    assert!(
        why.starts_with(&format!(
            "{} never reaches fbterm: ",
            chord("ctrl+shift+0").label(s.bindings().mac())
        )),
        "{why}"
    );
    assert!(why.ends_with(" or another program uses it"), "{why}");
    s.update(SettingsMsg::Withheld(None));
    let Some(RecordProblem::NotAllowed(why)) =
        &s.recording.as_ref().expect("still recording").problem
    else {
        panic!("{:?}", s.recording);
    };
    assert!(why.starts_with("That key never reaches fbterm"), "{why}");
}

#[test]
fn a_chord_that_cannot_be_bound_keeps_recording_and_says_why() {
    let mut s = state();
    s.update(SettingsMsg::Record(Action::NewWindow));
    for pressed in [
        Some(chord("q")),
        Some(chord("shift+q")),
        Some(chord("enter")),
        None,
    ] {
        assert_eq!(s.update(SettingsMsg::Pressed(pressed)), Effect::NONE);
        let r = s.recording.as_ref().expect("still recording");
        assert!(
            matches!(r.problem, Some(RecordProblem::NotAllowed(_))),
            "{pressed:?}: {r:?}"
        );
    }
    // F keys work alone.
    s.update(SettingsMsg::Pressed(Some(chord("f9"))));
    assert!(s.recording.is_none());
    assert_eq!(s.bindings().chord(Action::NewWindow), Some(chord("f9")));
}

#[test]
fn a_taken_chord_names_its_action_and_can_be_taken_over_or_left() {
    let mut s = state();
    let close = s.bindings().chord(Action::CloseTab).unwrap();
    s.update(SettingsMsg::Record(Action::NewTab));
    assert_eq!(s.update(SettingsMsg::Pressed(Some(close))), Effect::NONE);
    assert_eq!(
        s.recording.as_ref().unwrap().problem,
        Some(RecordProblem::Taken {
            chord: close,
            other: Action::CloseTab
        })
    );
    // Cancel: nothing changed.
    s.update(SettingsMsg::CancelRecording);
    assert!(s.recording.is_none());
    assert!(s.values.keys.map.is_empty());
    // Take over: the other action is left without a chord.
    s.update(SettingsMsg::Record(Action::NewTab));
    s.update(SettingsMsg::Pressed(Some(close)));
    let e = s.update(SettingsMsg::TakeOver);
    assert_eq!(e.write, Write::Now);
    let b = s.bindings();
    assert_eq!(b.chord(Action::NewTab), Some(close));
    assert_eq!(b.chord(Action::CloseTab), None);
    assert_eq!(b.action(close), Some(Action::NewTab));
    // Take over with nothing taken does nothing.
    assert_eq!(s.update(SettingsMsg::TakeOver), Effect::NONE);
}

#[test]
fn rows_reset_and_clear_and_restore_defaults_resets_all() {
    let mut s = state();
    let defaults = s.bindings();
    s.update(SettingsMsg::ClearKey(Action::Find));
    assert_eq!(s.bindings().chord(Action::Find), None);
    s.update(SettingsMsg::Record(Action::OpenSettingsFile));
    s.update(SettingsMsg::Pressed(Some(chord(FREE))));
    assert_eq!(s.values.keys.map.len(), 2);
    // Reset one row.
    s.update(SettingsMsg::ResetKey(Action::Find));
    assert!(s.bindings().is_default(Action::Find));
    // A row with no default chord resets to none.
    s.update(SettingsMsg::ResetKey(Action::OpenSettingsFile));
    assert!(s.values.keys.map.is_empty());
    // Resetting a row whose default another action took asks first.
    let palette = defaults.chord(Action::Palette).unwrap();
    s.update(SettingsMsg::Record(Action::NewWindow));
    s.update(SettingsMsg::Pressed(Some(palette)));
    s.update(SettingsMsg::TakeOver);
    assert_eq!(s.bindings().chord(Action::Palette), None);
    s.update(SettingsMsg::ResetKey(Action::Palette));
    assert!(matches!(
        s.recording.as_ref().unwrap().problem,
        Some(RecordProblem::Taken {
            other: Action::NewWindow,
            ..
        })
    ));
    s.update(SettingsMsg::CancelRecording);
    // Restore defaults.
    let e = s.update(SettingsMsg::ResetKeys);
    assert_eq!(e.apply, Some(SettingChange::Keys(Default::default())));
    assert_eq!(s.bindings(), defaults);
}

#[test]
fn a_read_only_window_records_nothing() {
    let err = ConfigError {
        path: "settings.toml".into(),
        line: None,
        message: "invalid".into(),
    };
    let mut s = SettingsState::new(&Settings::default(), Vec::new(), Some(err));
    s.update(SettingsMsg::Record(Action::NewTab));
    assert!(s.recording.is_none());
    assert_eq!(s.update(SettingsMsg::ResetKeys), Effect::NONE);
}

#[test]
fn chords_the_terminal_or_the_system_owns_are_refused() {
    let mut s = state();
    let mac = s.bindings().mac();
    let copy = if mac { "cmd+c" } else { "ctrl+shift+c" };
    s.update(SettingsMsg::Record(Action::NewTab));
    s.update(SettingsMsg::Pressed(Some(chord(copy))));
    let Some(RecordProblem::NotAllowed(why)) = &s.recording.as_ref().unwrap().problem else {
        panic!("{:?}", s.recording);
    };
    assert!(why.ends_with("is used by Copy"), "{why}");
    assert!(s.values.keys.map.is_empty());
    if mac {
        // Cmd+Q is Quit's even for another action, and Quit itself is fixed.
        s.update(SettingsMsg::Pressed(Some(chord("cmd+q"))));
        assert!(matches!(
            s.recording.as_ref().unwrap().problem,
            Some(RecordProblem::NotAllowed(_))
        ));
        s.update(SettingsMsg::CancelRecording);
        s.update(SettingsMsg::Record(Action::Quit));
        assert!(s.recording.is_none());
        assert_eq!(s.update(SettingsMsg::ClearKey(Action::Quit)), Effect::NONE);
    }
}

#[test]
fn an_edit_on_disk_ends_a_stale_take_over_question() {
    let mut s = state();
    let close = s.bindings().chord(Action::CloseTab).unwrap();
    s.update(SettingsMsg::Record(Action::NewTab));
    s.update(SettingsMsg::Pressed(Some(close)));
    assert!(s.recording.is_some());
    // The file changes the chords meanwhile: the question goes away.
    let mut disk = Settings::default();
    disk.keys.map.insert(Action::CloseTab, None);
    s.sync(&disk);
    assert!(s.recording.is_none());
    assert_eq!(s.update(SettingsMsg::TakeOver), Effect::NONE);
    // A sync that leaves the chords alone keeps recording.
    s.update(SettingsMsg::Record(Action::NewTab));
    s.sync(&disk);
    assert!(s.recording.is_some());
}

#[test]
fn take_over_checks_the_chord_is_still_taken() {
    let mut s = state();
    let close = s.bindings().chord(Action::CloseTab).unwrap();
    s.update(SettingsMsg::Record(Action::NewTab));
    s.update(SettingsMsg::Pressed(Some(close)));
    // The question stands, but Close tab let go of the chord in between.
    s.values.keys.map.insert(Action::CloseTab, None);
    let e = s.update(SettingsMsg::TakeOver);
    assert!(e.apply.is_some());
    let b = s.bindings();
    assert_eq!(b.chord(Action::NewTab), Some(close));
    assert_eq!(b.chord(Action::CloseTab), None);
}

/// A state whose saved scrollback is 10,000 and whose longest open tab holds `longest` lines.
fn with_tab(longest: usize) -> SettingsState {
    let mut s = state();
    assert_eq!(s.values.scrollback_lines, 10_000);
    s.max_open_history = longest;
    s
}

#[test]
fn lower_value_with_long_tab_asks_first() {
    let mut s = with_tab(8_000);
    let e = s.update(SettingsMsg::ScrollbackText("5000".into()));
    assert_eq!(e, Effect::NONE, "nothing is applied or written yet");
    assert_eq!(s.values.scrollback_lines, 10_000);
    assert!(!s.has_pending());
    assert_eq!(s.pending_lower, Some(5_000));
    assert_eq!(
        s.scrollback_warning().as_deref(),
        Some("Open tabs will lose their oldest lines beyond 5,000. This can't be undone.")
    );
    // Enter keeps the prompt; it never applies the lower value.
    let e = s.update(SettingsMsg::ScrollbackSubmit);
    assert_eq!(e, Effect::NONE);
    assert_eq!(s.pending_lower, Some(5_000));
    assert_eq!(s.values.scrollback_lines, 10_000);
}

#[test]
fn apply_saves_and_trims() {
    let mut s = with_tab(8_000);
    s.update(SettingsMsg::ScrollbackText("5000".into()));
    let e = s.update(SettingsMsg::ScrollbackApply);
    assert_eq!(e.apply, Some(SettingChange::ScrollbackLines(5_000)));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.values.scrollback_lines, 5_000);
    assert_eq!(s.scrollback_text, "5000");
    assert_eq!(s.pending_lower, None);
    assert_eq!(s.scrollback_warning(), None);
}

#[test]
fn cancel_restores_saved_value() {
    let mut s = with_tab(8_000);
    s.update(SettingsMsg::ScrollbackText("5000".into()));
    let e = s.update(SettingsMsg::ScrollbackCancel);
    assert_eq!(e, Effect::NONE);
    assert_eq!(s.pending_lower, None);
    assert_eq!(s.scrollback_text, "10000");
    assert_eq!(s.values.scrollback_lines, 10_000);
    assert_eq!(s.scrollback_warning(), None);
}

#[test]
fn lower_value_without_long_tab_saves_when_the_quiet_period_passes() {
    let mut s = with_tab(3_000);
    let e = s.update(SettingsMsg::ScrollbackText("5000".into()));
    assert_eq!(e.apply, None, "not per keystroke");
    assert_eq!(s.values.scrollback_lines, 10_000);
    let Write::After(n) = e.write else {
        panic!("a quiet period is scheduled: {e:?}");
    };
    let e = s.update(SettingsMsg::Flush(n));
    assert_eq!(e.apply, Some(SettingChange::ScrollbackLines(5_000)));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.pending_lower, None);
    assert_eq!(s.values.scrollback_lines, 5_000);
    // Enter applies at once, and a tab holding exactly the new value loses nothing either.
    let mut s = with_tab(5_000);
    let e = s.update(SettingsMsg::ScrollbackSubmit);
    assert_eq!(e.apply, None, "10000 is the saved value");
    s.update(SettingsMsg::ScrollbackText("5000".into()));
    let e = s.update(SettingsMsg::ScrollbackSubmit);
    assert!(e.apply.is_some());
    assert_eq!(s.pending_lower, None);
}

#[test]
fn typing_through_a_lower_value_no_tab_exceeds_never_applies() {
    let mut s = with_tab(300);
    for typed in ["2", "20", "200", "2000"] {
        let e = s.update(SettingsMsg::ScrollbackText(typed.into()));
        assert_eq!(e.apply, None, "{typed} must not apply");
        assert_eq!(s.values.scrollback_lines, 10_000);
    }
    // Earlier keystrokes' quiet periods are stale; raising applies at once.
    let e = s.update(SettingsMsg::Flush(0));
    assert_eq!(e.apply, None);
    let e = s.update(SettingsMsg::ScrollbackText("20000".into()));
    assert_eq!(e.apply, Some(SettingChange::ScrollbackLines(20_000)));
    assert!(!s.holds_lower());
}

#[test]
fn typing_through_lower_value_never_trims() {
    let mut s = with_tab(8_000);
    for typed in ["5", "50", "500", "5000"] {
        let e = s.update(SettingsMsg::ScrollbackText(typed.into()));
        assert_eq!(e.apply, None, "{typed} must not apply");
        assert_eq!(s.values.scrollback_lines, 10_000);
        assert!(!s.has_pending());
    }
    let e = s.update(SettingsMsg::ScrollbackText("50000".into()));
    assert_eq!(e.apply, Some(SettingChange::ScrollbackLines(50_000)));
    assert_eq!(s.pending_lower, None);
    assert_eq!(s.values.scrollback_lines, 50_000);
    // A typed value that stops being a number withdraws the question.
    let mut s = with_tab(8_000);
    s.update(SettingsMsg::ScrollbackText("5000".into()));
    s.update(SettingsMsg::ScrollbackText("".into()));
    assert_eq!(s.pending_lower, None);
}

#[test]
fn large_value_shows_note_and_saves() {
    let mut s = state();
    assert_eq!(s.scrollback_note(), None);
    let e = s.update(SettingsMsg::ScrollbackText("150,000".into()));
    assert_eq!(e.apply, Some(SettingChange::ScrollbackLines(150_000)));
    assert_eq!(
        s.scrollback_note(),
        Some("Large history: resizing a window can pause briefly while history rewraps.")
    );
}

#[test]
fn note_hidden_at_threshold() {
    let mut s = state();
    s.update(SettingsMsg::ScrollbackText("100000".into()));
    assert_eq!(s.values.scrollback_lines, 100_000);
    assert_eq!(s.scrollback_note(), None);
    s.update(SettingsMsg::ScrollbackText("100001".into()));
    assert!(s.scrollback_note().is_some());
}

#[test]
fn thousands_separates_groups_of_three() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(2_000), "2,000");
    assert_eq!(thousands(100_000), "100,000");
    assert_eq!(thousands(1_000_000), "1,000,000");
}

#[test]
fn retries_field_writes_setting() {
    let mut s = state();
    assert_eq!(s.retries_text, "10");
    // Typing applies in range, after a quiet period.
    let e = s.update(SettingsMsg::RetriesText("5".into()));
    assert_eq!(e.apply, Some(SettingChange::TunnelMaxRetries(5)));
    assert!(matches!(e.write, Write::After(_)));
    assert_eq!(s.values.tunnel_max_retries, 5);
    // 0 is allowed: it turns automatic reconnecting off.
    let e = s.update(SettingsMsg::RetriesText("0".into()));
    assert_eq!(e.apply, Some(SettingChange::TunnelMaxRetries(0)));
    // Out of range waits for Enter, which clamps.
    assert_eq!(
        s.update(SettingsMsg::RetriesText("500".into())),
        Effect::NONE
    );
    assert_eq!(s.values.tunnel_max_retries, 0);
    let e = s.update(SettingsMsg::RetriesSubmit);
    assert_eq!(e.apply, Some(SettingChange::TunnelMaxRetries(100)));
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.retries_text, "100");
    // Garbage goes back to the current value.
    s.update(SettingsMsg::RetriesText("many".into()));
    s.update(SettingsMsg::RetriesSubmit);
    assert_eq!(s.retries_text, "100");
}

#[test]
fn a_fixed_shortcut_row_shows_its_chord_and_cannot_be_recorded() {
    use crate::app::Message;
    use fbterm_config::keys::fixed_shortcuts;
    use iced::advanced::layout::{self, Layout};
    use iced::advanced::widget::{Operation, Tree};
    use iced::advanced::{Shell, clipboard};
    use iced::{Element, Event, Pixels, Point, Rectangle, Size, mouse};

    struct Texts(Vec<String>);
    impl Operation for Texts {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _id: Option<&iced::advanced::widget::Id>, _b: Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }
    let t = crate::theme::graphite_tokens();
    let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        Pixels(16.0),
    ));
    let size = Size::new(560.0, 80.0);
    for mac in [false, true] {
        let shortcut = fixed_shortcuts(mac)[4];
        let mut element: Element<'_, Message> = view::fixed_key_row(t, shortcut, mac);
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut texts = Texts(Vec::new());
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut texts);
        let mut expected = vec![
            "Scroll to top".to_owned(),
            "Fixed".to_owned(),
            shortcut.chord.label(mac),
        ];
        if mac {
            expected.push("Also \u{2318}Home.".to_owned());
        }
        assert_eq!(texts.0, expected);
        // A click on the chord records nothing.
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let at = Point::new(size.width - 16.0 - 40.0, 17.0);
        for event in [
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                mouse::Cursor::Available(at),
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(size),
            );
        }
        assert!(messages.is_empty(), "{messages:?}");
    }
}

#[test]
fn the_restart_note_needs_a_lower_opacity_and_an_opaque_window() {
    let note = Some("Restart fbterm to apply this to open windows.");
    assert_eq!(view::restart_note(0.8, true), note);
    assert_eq!(view::restart_note(1.0, true), None);
    assert_eq!(view::restart_note(0.8, false), None);
}

#[test]
fn the_group_tabs_row_shows_only_with_tabs_on_the_left() {
    use fbterm_config::TabBar;
    assert!(!view::shows_group_tabs_row(&Settings::default()));
    let top = Settings {
        tab_bar: Some(TabBar::Top),
        ..Settings::default()
    };
    assert!(!view::shows_group_tabs_row(&top));
    let left = Settings {
        tab_bar: Some(TabBar::Left),
        ..Settings::default()
    };
    assert!(view::shows_group_tabs_row(&left));
}

fn with_scrollback(lines: usize) -> SettingsState {
    let mut s = state();
    s.update(SettingsMsg::ScrollbackText(lines.to_string()));
    s.update(SettingsMsg::ScrollbackSubmit);
    assert_eq!(s.values.scrollback_lines, lines);
    s.take_pending();
    s
}

#[test]
fn compression_switch_saves() {
    let mut s = state();
    assert!(!s.values.history_compression);
    let e = s.update(SettingsMsg::HistoryCompression(true));
    assert_eq!(e.apply, Some(SettingChange::HistoryCompression(true)));
    assert_eq!(e.write, Write::Now);
    assert!(s.values.history_compression);
    assert_eq!(s.take_pending(), [SettingChange::HistoryCompression(true)]);
}

#[test]
fn uncompressed_field_disabled_while_off() {
    let mut s = state();
    assert!(!s.uncompressed_enabled());
    assert_eq!(
        s.uncompressed_note(),
        None,
        "off is not a scrollback problem"
    );
    // The field shows the effective default while the setting is absent.
    assert_eq!(s.uncompressed_text, "9999");
    s.update(SettingsMsg::HistoryCompression(true));
    assert!(s.uncompressed_enabled());
}

#[test]
fn uncompressed_field_disabled_with_small_scrollback() {
    let mut s = with_scrollback(1_000);
    s.update(SettingsMsg::HistoryCompression(true));
    assert!(!s.uncompressed_enabled());
    assert_eq!(
        s.uncompressed_note(),
        Some("Needs scrollback above 1,000 lines.")
    );
    assert_eq!(s.uncompressed_text, "999");
    let mut s = with_scrollback(1_001);
    s.update(SettingsMsg::HistoryCompression(true));
    assert!(s.uncompressed_enabled());
    assert_eq!(s.uncompressed_note(), None);
}

#[test]
fn uncompressed_in_range_saves_and_shows_default_when_absent() {
    let mut s = state();
    s.update(SettingsMsg::HistoryCompression(true));
    s.take_pending();
    // Absent: the field shows the default, kept below the scrollback of 10,000.
    assert_eq!(s.values.uncompressed_history_lines, None);
    assert_eq!(s.uncompressed_text, "9999");
    let e = s.update(SettingsMsg::UncompressedText("5,000".into()));
    assert_eq!(e.apply, None, "typing waits for Enter or the quiet period");
    let Write::After(n) = e.write else {
        panic!("a typed value waits: {e:?}");
    };
    assert_eq!(s.uncompressed_error(), None);
    let e = s.update(SettingsMsg::Flush(n));
    assert_eq!(
        e.apply,
        Some(SettingChange::UncompressedHistoryLines(5_000))
    );
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.values.uncompressed_history_lines, Some(5_000));
    assert_eq!(
        s.take_pending(),
        [SettingChange::UncompressedHistoryLines(5_000)]
    );
}

/// A state whose saved settings have `scrollback` lines and `uncompressed_history_lines` set
/// to `keep`, with compression off.
fn with_stored_uncompressed(scrollback: usize, keep: usize) -> SettingsState {
    let mut s = state();
    s.sync(&Settings {
        scrollback_lines: scrollback,
        uncompressed_history_lines: Some(keep),
        ..Settings::default()
    });
    s
}

/// A stored value out of range is kept as written while compression is off; switching it on
/// in the tab clamps it into range and saves that, so no error shows under the field and no
/// notice comes at the next launch.
#[test]
fn switching_on_normalizes_an_out_of_range_stored_value() {
    for (keep, want) in [(8_000, 4_999), (200, 1_000)] {
        let mut s = with_stored_uncompressed(5_000, keep);
        let e = s.update(SettingsMsg::HistoryCompression(true));
        assert_eq!(e.apply, Some(SettingChange::HistoryCompression(true)));
        assert_eq!(e.write, Write::Now);
        assert_eq!(s.values.uncompressed_history_lines, Some(want), "{keep}");
        assert_eq!(s.uncompressed_text, want.to_string());
        assert_eq!(s.uncompressed_error(), None);
    }
    // In range: left as it is.
    let mut s = with_stored_uncompressed(5_000, 2_000);
    s.update(SettingsMsg::HistoryCompression(true));
    assert_eq!(s.values.uncompressed_history_lines, Some(2_000));
}

/// Typed text that is not saved (and its error) does not survive switching off and on.
#[test]
fn toggling_compression_resyncs_the_uncompressed_field() {
    let mut s = state();
    s.update(SettingsMsg::HistoryCompression(true));
    s.update(SettingsMsg::UncompressedText("500".into()));
    assert!(s.uncompressed_error().is_some());
    s.update(SettingsMsg::HistoryCompression(false));
    assert_eq!(s.uncompressed_text, "9999");
    s.update(SettingsMsg::HistoryCompression(true));
    assert_eq!(s.uncompressed_text, "9999");
    assert_eq!(s.uncompressed_error(), None);
    // A value waiting on its quiet period is dropped too.
    s.update(SettingsMsg::UncompressedText("2000".into()));
    s.update(SettingsMsg::HistoryCompression(false));
    s.take_pending();
    for n in 0..=s.generation {
        assert_eq!(s.update(SettingsMsg::Flush(n)).apply, None);
    }
    assert_eq!(s.values.uncompressed_history_lines, None);
}

/// The view leaves the disabled field read-only; the handlers ignore its messages as well.
#[test]
fn a_disabled_uncompressed_field_ignores_input() {
    for mut s in [state(), {
        let mut s = with_scrollback(1_000);
        s.update(SettingsMsg::HistoryCompression(true));
        s.take_pending();
        s
    }] {
        assert!(!s.uncompressed_enabled());
        let shown = s.uncompressed_text.clone();
        let values = s.values.clone();
        assert_eq!(
            s.update(SettingsMsg::UncompressedText("5000".into())),
            Effect::NONE
        );
        assert_eq!(s.uncompressed_text, shown);
        assert_eq!(s.update(SettingsMsg::UncompressedSubmit), Effect::NONE);
        assert_eq!(s.uncompressed_text, shown);
        assert_eq!(s.values, values);
        assert!(!s.has_pending());
    }
}

/// With a scrollback of 1,000 or less the field is disabled and shows the clamped effective
/// value, never a stored one above the scrollback.
#[test]
fn a_small_scrollback_shows_the_clamped_value() {
    let s = with_stored_uncompressed(800, 8_000);
    assert_eq!(s.uncompressed_text, "799");
    let mut s = with_stored_uncompressed(5_000, 2_000);
    s.update(SettingsMsg::ScrollbackText("900".into()));
    s.update(SettingsMsg::ScrollbackSubmit);
    assert_eq!(s.values.scrollback_lines, 900);
    assert_eq!(s.uncompressed_text, "899");
}

/// A typed value still waiting on its quiet period is applied when the tab closes, as Enter
/// would; an out-of-range one is not.
#[test]
fn closing_applies_a_held_uncompressed_value() {
    let mut s = state();
    s.update(SettingsMsg::HistoryCompression(true));
    s.take_pending();
    s.update(SettingsMsg::UncompressedText("5000".into()));
    let e = s.flush_typed();
    assert_eq!(
        e.apply,
        Some(SettingChange::UncompressedHistoryLines(5_000))
    );
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.flush_typed(), Effect::NONE);
    s.take_pending();
    s.update(SettingsMsg::UncompressedText("50000".into()));
    assert_eq!(s.flush_typed().apply, None);
    assert!(!s.has_pending());
}

/// Typing `20000` over `10000` one key at a time applies none of `2`, `20`, `200`, `2000` (a
/// lowering would compress every tab and ask for a memory release, and each apply makes open
/// find bars search again): only the value standing at Enter, or when the quiet period after
/// the last key passes, applies, once.
#[test]
fn typing_the_uncompressed_value_applies_once() {
    let typed = |s: &mut SettingsState| {
        let mut generations = Vec::new();
        for text in ["2", "20", "200", "2000", "20000"] {
            let e = s.update(SettingsMsg::UncompressedText(text.into()));
            assert_eq!(e.apply, None, "{text} applied while typing");
            if let Write::After(n) = e.write {
                generations.push(n);
            }
        }
        generations
    };
    let fresh = || {
        let mut s = with_scrollback(100_000);
        s.update(SettingsMsg::HistoryCompression(true));
        assert_eq!(s.uncompressed_text, "10000");
        s.take_pending();
        s
    };

    // Enter applies the value once.
    let mut s = fresh();
    let generations = typed(&mut s);
    let e = s.update(SettingsMsg::UncompressedSubmit);
    assert_eq!(
        e.apply,
        Some(SettingChange::UncompressedHistoryLines(20_000))
    );
    assert_eq!(e.write, Write::Now);
    // The quiet periods of the keys typed then apply nothing more.
    for n in generations {
        assert_eq!(s.update(SettingsMsg::Flush(n)).apply, None);
    }

    // Without Enter: only the last key's quiet period applies, once.
    let mut s = fresh();
    let generations = typed(&mut s);
    let (last, older) = generations.split_last().expect("typed values wait");
    for &n in older {
        assert_eq!(s.update(SettingsMsg::Flush(n)).apply, None);
    }
    let e = s.update(SettingsMsg::Flush(*last));
    assert_eq!(
        e.apply,
        Some(SettingChange::UncompressedHistoryLines(20_000))
    );
    assert_eq!(e.write, Write::Now);
    assert_eq!(s.update(SettingsMsg::Flush(*last)).apply, None);
    assert_eq!(s.values.uncompressed_history_lines, Some(20_000));
}

#[test]
fn uncompressed_out_of_range_saves_nothing() {
    let mut s = state();
    s.update(SettingsMsg::HistoryCompression(true));
    s.take_pending();
    for typed in ["500", "10000", "20000"] {
        let e = s.update(SettingsMsg::UncompressedText(typed.into()));
        assert_eq!(e, Effect::NONE);
        assert_eq!(
            s.uncompressed_error().as_deref(),
            Some("Enter a number from 1,000 to 9,999.")
        );
        let e = s.update(SettingsMsg::UncompressedSubmit);
        assert_eq!(e.apply, None);
        assert!(!s.has_pending());
        assert_eq!(s.values.uncompressed_history_lines, None);
    }
    // A valid value clears the message.
    s.update(SettingsMsg::UncompressedText("2000".into()));
    assert_eq!(s.uncompressed_error(), None);
}

#[test]
fn confirmation_mentions_uncompressed_drop() {
    let mut s = with_tab(8_000);
    s.update(SettingsMsg::HistoryCompression(true));
    s.update(SettingsMsg::UncompressedText("9000".into()));
    s.update(SettingsMsg::UncompressedSubmit);
    assert_eq!(s.values.uncompressed_history_lines, Some(9_000));
    s.update(SettingsMsg::ScrollbackText("5000".into()));
    assert_eq!(
        s.scrollback_warning().as_deref(),
        Some(
            "Open tabs will lose their oldest lines beyond 5,000. This can't be undone.\nRecent lines kept uncompressed will drop to 4,999."
        )
    );
    // Applying drags the stored value and the field follows.
    s.update(SettingsMsg::ScrollbackApply);
    assert_eq!(s.values.uncompressed_history_lines, Some(4_999));
    assert_eq!(s.uncompressed_text, "4999");
}

#[test]
fn confirmation_has_no_extra_line_when_nothing_drops() {
    let mut s = with_tab(8_000);
    s.update(SettingsMsg::HistoryCompression(true));
    s.update(SettingsMsg::UncompressedText("2000".into()));
    s.update(SettingsMsg::UncompressedSubmit);
    s.update(SettingsMsg::ScrollbackText("5000".into()));
    assert_eq!(
        s.scrollback_warning().as_deref(),
        Some("Open tabs will lose their oldest lines beyond 5,000. This can't be undone.")
    );
}
