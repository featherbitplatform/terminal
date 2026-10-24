use std::collections::BTreeMap;
use std::path::PathBuf;

use fbterm_config::*;

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fbterm-config-test-{}", ProfileId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn ssh(name: &str, jump: Vec<ProfileId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: format!("{name}.example.com"),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Agent,
            jump,
        }),
    }
}

fn local(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: Some("shells".into()),
        accent: Some(Rgb {
            r: 0xAB,
            g: 0x0C,
            b: 0xff,
        }),
        kind: ProfileKind::Local(LocalProfile {
            program: "/bin/zsh".into(),
            args: vec!["-l".into()],
            cwd: None,
            env: BTreeMap::new(),
        }),
    }
}

#[test]
fn round_trip_profiles_toml() {
    let path = tmp("profiles.toml");
    let mut key = ssh("key", vec![]);
    if let ProfileKind::Ssh(s) = &mut key.kind {
        s.auth = AuthMethod::Key {
            path: "/k".into(),
            passphrase: true,
        };
    }
    let store = ProfileStore {
        profiles: vec![local("l"), ssh("pw", vec![]), key],
    };
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("password"));
    assert!(text.contains("#ab0cff"));
    assert_eq!(ProfileStore::load(&path).unwrap(), store);
}

#[test]
fn ssh_port_defaults_to_22() {
    let path = tmp("profiles.toml");
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\nkind = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nauth = { type = \"agent\" }\n",
    )
    .unwrap();
    let store = ProfileStore::load(&path).unwrap();
    match &store.profiles[0].kind {
        ProfileKind::Ssh(s) => assert_eq!(s.port, 22),
        _ => panic!("expected ssh"),
    }
}

#[test]
fn syntax_error_reports_line_and_keeps_file() {
    let path = tmp("profiles.toml");
    std::fs::write(&path, "[[profiles]\n").unwrap();
    let err = ProfileStore::load(&path).unwrap_err();
    assert_eq!(err.line, Some(1));
    assert_eq!(err.path, path);
    assert!(err.to_string().contains(":1:"));
    assert_eq!(std::fs::read(&path).unwrap(), b"[[profiles]\n");
}

#[test]
fn dangling_jump_is_a_validation_error() {
    let missing = ProfileId::new();
    let store = ProfileStore {
        profiles: vec![ssh("a", vec![missing])],
    };
    assert!(
        store
            .validate()
            .iter()
            .any(|e| matches!(e, ValidationError::MissingJump { missing: m, .. } if *m == missing))
    );
}

#[test]
fn jump_cycle_detected() {
    let mut a = ssh("a", vec![]);
    let b = ssh("b", vec![a.id]);
    if let ProfileKind::Ssh(s) = &mut a.kind {
        s.jump = vec![b.id];
    }
    let store = ProfileStore {
        profiles: vec![a.clone(), b],
    };
    assert!(
        store
            .validate()
            .iter()
            .any(|e| matches!(e, ValidationError::JumpCycle { .. }))
    );
    assert!(matches!(
        store.jump_chain(a.id),
        Err(ValidationError::JumpCycle { .. })
    ));
}

#[test]
fn jump_chain_order_two_hops() {
    let b1 = ssh("b1", vec![]);
    let b2 = ssh("b2", vec![]);
    let t = ssh("t", vec![b1.id, b2.id]);
    let tid = t.id;
    let store = ProfileStore {
        profiles: vec![t, b1.clone(), b2.clone()],
    };
    let chain: Vec<_> = store
        .jump_chain(tid)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(chain, vec![b1.id, b2.id]);
    assert!(store.validate().is_empty());
}

#[test]
fn jump_to_local_profile_rejected() {
    let l = local("l");
    let t = ssh("t", vec![l.id]);
    let tid = t.id;
    let store = ProfileStore {
        profiles: vec![t, l],
    };
    assert!(
        store
            .validate()
            .iter()
            .any(|e| matches!(e, ValidationError::JumpNotSsh { .. }))
    );
    assert!(matches!(
        store.jump_chain(tid),
        Err(ValidationError::JumpNotSsh { .. })
    ));
}

#[test]
fn duplicate_name_and_empty_host() {
    let mut a = ssh("a", vec![]);
    if let ProfileKind::Ssh(s) = &mut a.kind {
        s.host.clear();
    }
    let store = ProfileStore {
        profiles: vec![a, ssh("a", vec![])],
    };
    let errs = store.validate();
    assert!(errs.contains(&ValidationError::DuplicateName("a".into())));
    assert!(errs.contains(&ValidationError::EmptyHost("a".into())));
}

#[test]
fn rgb_parses_upper_and_rejects_invalid() {
    let ok: Rgb = serde_json_free("#AABBCC").unwrap();
    assert_eq!(
        ok,
        Rgb {
            r: 0xaa,
            g: 0xbb,
            b: 0xcc
        }
    );
    assert!(serde_json_free("AABBCC").is_err());
    assert!(serde_json_free("#GGGGGG").is_err());
}

fn serde_json_free(s: &str) -> Result<Rgb, toml::de::Error> {
    #[derive(serde::Deserialize)]
    struct W {
        c: Rgb,
    }
    toml::from_str::<W>(&format!("c = \"{s}\"")).map(|w| w.c)
}

#[test]
fn missing_settings_file_gives_defaults() {
    let s = Settings::load_or_default(&tmp("nope.toml"));
    assert_eq!(s.font_size, 13.0);
    assert_eq!(s.scrollback_lines, 10_000);
    assert_eq!(s.font_family, "Geist Mono");
    assert_eq!(s.theme, ThemeMode::Dark);
}

#[test]
fn partial_settings_file_uses_defaults() {
    let path = tmp("settings.toml");
    std::fs::write(&path, "theme = \"light\"\n").unwrap();
    let s = Settings::load_or_default(&path);
    assert_eq!(s.theme, ThemeMode::Light);
    assert_eq!(s.font_size, 13.0);
    assert!(
        !s.kitty_keyboard,
        "kitty keyboard protocol is off by default"
    );
}

#[test]
fn kitty_keyboard_setting_parses() {
    let path = tmp("settings.toml");
    std::fs::write(&path, "kitty_keyboard = true\n").unwrap();
    let s = Settings::load_or_default(&path);
    assert!(s.kitty_keyboard);
    assert_eq!(s.scrollback_lines, 10_000);
}

#[test]
fn save_creates_missing_directory() {
    let path = tmp("sub").join("deep").join("profiles.toml");
    ProfileStore::default().save(&path).unwrap();
    assert!(path.exists());
}

#[test]
fn paths_filenames() {
    let p = Paths {
        config_dir: "/x".into(),
    };
    assert_eq!(p.profiles(), PathBuf::from("/x/profiles.toml"));
    assert_eq!(p.settings(), PathBuf::from("/x/settings.toml"));
    assert_eq!(p.state(), PathBuf::from("/x/state.toml"));
}

#[test]
fn broken_settings_reports_line() {
    let path = tmp("settings.toml");
    let bytes = b"theme = \"dark\"\nfont_size = [\n";
    std::fs::write(&path, bytes).unwrap();
    let err = Settings::load(&path).unwrap_err();
    assert!(matches!(err.line, Some(n) if n >= 2));
    assert_eq!(err.path, path);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn missing_settings_load_is_ok_defaults() {
    assert_eq!(
        Settings::load(&tmp("nope.toml")).unwrap(),
        Settings::default()
    );
}

#[test]
fn out_of_range_settings_are_clamped_and_reported() {
    let path = tmp("settings.toml");
    std::fs::write(
        &path,
        "theme = \"light\"\nfont_size = 200.0\nscrollback_lines = 5000000\n",
    )
    .unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.font_size, 72.0);
    assert_eq!(s.scrollback_lines, 1_000_000);
    assert_eq!(s.theme, ThemeMode::Light);
    let w = warning.expect("out-of-range values are reported");
    assert_eq!(w.path, path);
    assert_eq!(w.line, Some(2));
    assert!(w.message.contains("font_size"), "{}", w.message);
    assert!(w.message.contains("scrollback_lines"), "{}", w.message);
    // `load` applies the same clamping, without the warning.
    assert_eq!(Settings::load(&path).unwrap(), s);
}

#[test]
fn small_font_size_is_raised_to_the_minimum() {
    let path = tmp("settings.toml");
    std::fs::write(&path, "font_size = 1.5\n").unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.font_size, 6.0);
    let w = warning.unwrap();
    assert_eq!(w.line, Some(1));
    assert!(w.message.contains("font_size 1.5"), "{}", w.message);
}

#[test]
fn in_range_settings_load_without_warning() {
    let path = tmp("settings.toml");
    std::fs::write(&path, "font_size = 6.0\nscrollback_lines = 0\n").unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!((s.font_size, s.scrollback_lines), (6.0, 0));
    assert!(warning.is_none());
}

#[test]
fn non_finite_font_size_takes_the_default() {
    let mut s = Settings {
        font_size: f32::NAN,
        ..Settings::default()
    };
    let problems = s.clamp();
    assert_eq!(s.font_size, 13.0);
    assert_eq!(problems[0].0, "font_size");
}

#[test]
fn background_settings_default_to_opaque_without_blur() {
    let s = Settings::load_or_default(&tmp("nope.toml"));
    assert_eq!(s.background_opacity, 1.0);
    assert!(!s.background_blur);
}

#[test]
fn background_settings_parse() {
    let path = tmp("settings.toml");
    std::fs::write(&path, "background_opacity = 0.85\nbackground_blur = true\n").unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.background_opacity, 0.85);
    assert!(s.background_blur);
    assert!(warning.is_none());
}

#[test]
fn out_of_range_opacity_is_clamped_and_reported() {
    let path = tmp("settings.toml");
    std::fs::write(&path, "theme = \"dark\"\nbackground_opacity = 1.5\n").unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.background_opacity, 1.0);
    let w = warning.expect("an out-of-range opacity is reported");
    assert_eq!(w.line, Some(2));
    assert!(
        w.message.contains("background_opacity 1.5"),
        "{}",
        w.message
    );

    // Fully clear is not allowed: the floor is 0.05.
    for value in ["-0.2", "0.0", "0.01"] {
        std::fs::write(&path, format!("background_opacity = {value}\n")).unwrap();
        let (s, warning) = Settings::load_checked(&path).unwrap();
        assert_eq!(s.background_opacity, 0.05, "{value}");
        assert_eq!(warning.expect(value).line, Some(1));
    }
    std::fs::write(&path, "background_opacity = 0.05\n").unwrap();
    assert!(Settings::load_checked(&path).unwrap().1.is_none());

    let mut s = Settings {
        background_opacity: f32::NAN,
        ..Settings::default()
    };
    let problems = s.clamp();
    assert_eq!(s.background_opacity, 1.0);
    assert_eq!(problems[0].0, "background_opacity");
}

#[test]
fn max_fps_defaults_to_60_and_takes_a_number_or_display() {
    assert_eq!(Settings::default().max_fps, MaxFps::Fixed(60));
    let path = tmp("settings.toml");
    for (text, want) in [
        ("max_fps = 120\n", MaxFps::Fixed(120)),
        ("max_fps = 30\n", MaxFps::Fixed(30)),
        ("max_fps = 240\n", MaxFps::Fixed(240)),
        ("max_fps = \"display\"\n", MaxFps::Display),
    ] {
        std::fs::write(&path, text).unwrap();
        let (s, warning) = Settings::load_checked(&path).unwrap();
        assert_eq!(s.max_fps, want, "{text}");
        assert!(warning.is_none(), "{text}");
    }
    // Saved settings read back the same, in both forms.
    for v in [MaxFps::Fixed(90), MaxFps::Display] {
        let s = Settings {
            max_fps: v.clone(),
            ..Settings::default()
        };
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s, "{text}");
    }
}

#[test]
fn out_of_range_max_fps_is_clamped_and_reported() {
    let path = tmp("settings.toml");
    for (value, used) in [
        ("10", 30),
        ("0", 30),
        ("-5", 30),
        ("241", 240),
        ("100000", 240),
    ] {
        std::fs::write(&path, format!("theme = \"dark\"\nmax_fps = {value}\n")).unwrap();
        let (s, warning) = Settings::load_checked(&path).unwrap();
        assert_eq!(s.max_fps, MaxFps::Fixed(used), "{value}");
        let w = warning.expect(value);
        assert_eq!(w.line, Some(2));
        assert!(
            w.message.contains(&format!(
                "max_fps {value} is out of range (30 to 240), using {used}"
            )),
            "{}",
            w.message
        );
    }
}

#[test]
fn invalid_max_fps_is_reported_and_the_default_used() {
    let path = tmp("settings.toml");
    for value in ["\"fast\"", "\"Display\"", "59.5", "true"] {
        std::fs::write(&path, format!("font_size = 14.0\nmax_fps = {value}\n")).unwrap();
        let (s, warning) = Settings::load_checked(&path).unwrap();
        assert_eq!(s.max_fps, MaxFps::Fixed(60), "{value}");
        // The rest of the file still applies.
        assert_eq!(s.font_size, 14.0);
        let w = warning.expect(value);
        assert_eq!(w.line, Some(2));
        assert!(w.message.contains("max_fps"), "{}", w.message);
        assert!(w.message.contains("using 60"), "{}", w.message);
    }
    // A table is the wrong type altogether: the file does not load.
    std::fs::write(&path, "max_fps = [60]\n").unwrap();
    assert!(Settings::load_checked(&path).is_err());
}

#[test]
fn effective_max_fps_follows_the_display_within_the_range() {
    assert_eq!(MaxFps::Fixed(90).effective(Some(144)), 90);
    assert_eq!(MaxFps::Fixed(10).effective(None), 30);
    assert_eq!(MaxFps::Display.effective(Some(120)), 120);
    assert_eq!(MaxFps::Display.effective(Some(500)), 240);
    assert_eq!(MaxFps::Display.effective(Some(24)), 30);
    // Unknown (or reported as 0): the default.
    assert_eq!(MaxFps::Display.effective(None), 60);
    assert_eq!(MaxFps::Display.effective(Some(0)), 60);
}

#[test]
fn settings_template_parses_to_defaults_as_is() {
    let path = tmp("settings.toml");
    std::fs::write(&path, SETTINGS_TEMPLATE).unwrap();
    let (loaded, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(loaded, Settings::default());
    assert!(warning.is_none());
}

#[test]
fn settings_template_parses_to_defaults_with_every_setting_uncommented() {
    let uncommented: String = SETTINGS_TEMPLATE
        .lines()
        .map(|l| {
            // Setting lines are `#name = value` (and the `#[keys]` table); prose lines start
            // with `# `.
            match l.strip_prefix('#') {
                Some(rest) if rest.starts_with(|c: char| c.is_ascii_lowercase() || c == '[') => {
                    rest
                }
                _ => l,
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let path = tmp("settings.toml");
    std::fs::write(&path, &uncommented).unwrap();
    let (loaded, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(loaded, Settings::default());
    assert!(warning.is_none());
    // Every field of Settings is written out.
    let table: toml::Table = toml::from_str(&uncommented).unwrap();
    let defaults: toml::Table =
        toml::from_str(&toml::to_string(&Settings::default()).unwrap()).unwrap();
    let mut keys: Vec<_> = table.keys().collect();
    let mut want: Vec<_> = defaults.keys().collect();
    keys.sort();
    want.sort();
    assert_eq!(keys, want);
}

#[test]
fn writing_the_template_never_overwrites_a_file() {
    let path = tmp("settings.toml");
    assert!(Settings::write_template_if_missing(&path).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SETTINGS_TEMPLATE);
    std::fs::write(&path, "font_size = 20.0\n").unwrap();
    assert!(!Settings::write_template_if_missing(&path).unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "font_size = 20.0\n"
    );
}

#[cfg(unix)]
#[test]
fn writing_the_template_leaves_a_dangling_symlink_alone() {
    let path = tmp("settings.toml");
    let target = path.with_file_name("elsewhere.toml");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(!Settings::write_template_if_missing(&path).unwrap());
    assert!(!target.exists());
    assert_eq!(std::fs::read_link(&path).unwrap(), target);
}

#[test]
fn key_overrides_load_and_bad_entries_are_reported_and_use_the_default() {
    use fbterm_config::keys::{Action, Bindings, Chord};
    let path = tmp("settings.toml");
    std::fs::write(
        &path,
        "font_size = 14.0\n\
         \n\
         [keys]\n\
         new_tab = \"ctrl+alt+shift+t\"\n\
         close_tab = \"none\"\n\
         nwe_window = \"ctrl+alt+n\"\n\
         quit = \"q\"\n\
         find = \"ctrl+shift+f+g\"\n\
         palette = 3\n",
    )
    .unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.font_size, 14.0, "the rest of the file still applies");
    let chord = |t: &str| Chord::parse(t).unwrap();
    assert_eq!(
        s.keys.map.get(&Action::NewTab),
        Some(&chord("ctrl+alt+shift+t"))
    );
    assert_eq!(s.keys.map.get(&Action::CloseTab), Some(&None));
    assert_eq!(s.keys.map.len(), 2, "{:?}", s.keys.map);
    assert!(s.keys.invalid.is_empty());
    let b = Bindings::current(&s.keys.map);
    assert!(
        b.is_default(Action::Quit) && b.is_default(Action::Find) && b.is_default(Action::Palette)
    );
    let w = warning.expect("bad entries are reported");
    for part in [
        "keys.find \"ctrl+shift+f+g\" has an unknown modifier \"f\"",
        "keys.nwe_window is not an action",
        "keys.palette 3 is not a chord",
        "keys.quit \"q\": needs Ctrl, Alt or Cmd",
    ] {
        assert!(w.message.contains(part), "{part}: {}", w.message);
    }
    // The line of the first problem (entries are checked by name).
    assert_eq!(w.line, Some(8));
}

#[test]
fn a_key_override_on_another_actions_chord_falls_back_with_a_notice() {
    use fbterm_config::keys::{Action, Bindings, default_chord};
    let mac = cfg!(target_os = "macos");
    let close = default_chord(mac, Action::CloseTab).unwrap().to_config(mac);
    let path = tmp("settings.toml");
    std::fs::write(&path, format!("[keys]\nnew_tab = \"{close}\"\n")).unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert!(s.keys.map.is_empty());
    assert!(Bindings::current(&s.keys.map).is_default(Action::NewTab));
    let w = warning.unwrap();
    assert!(
        w.message
            .contains("is already used by close_tab, using the default"),
        "{}",
        w.message
    );
    assert_eq!(w.line, Some(2));
}

#[test]
fn a_key_override_on_a_reserved_chord_falls_back_with_a_notice() {
    use fbterm_config::keys::Action;
    let mac = cfg!(target_os = "macos");
    let copy = if mac { "cmd+c" } else { "ctrl+shift+c" };
    let path = tmp("settings.toml");
    std::fs::write(&path, format!("[keys]\nfind = \"{copy}\"\n")).unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert!(!s.keys.map.contains_key(&Action::Find));
    let w = warning.unwrap();
    assert!(
        w.message.contains("is used by Copy, using the default"),
        "{}",
        w.message
    );
    assert_eq!(w.line, Some(2));
}
