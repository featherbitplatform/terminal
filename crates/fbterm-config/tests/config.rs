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
        icon: None,
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: format!("{name}.example.com"),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Agent,
            jump,
            tunnels: Vec::new(),
        }),
    }
}

fn local(name: &str) -> Profile {
    Profile {
        icon: None,
        id: ProfileId::new(),
        name: name.into(),
        group: Some("shells".into()),
        accent: Some(Rgb {
            r: 0xAB,
            g: 0x0C,
            b: 0xff,
        }),
        accent_style: Default::default(),
        color_scheme: None,
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
            path: Some("/k".into()),
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
fn key_without_path_round_trips() {
    let path = tmp("profiles.toml");
    let mut key = ssh("shared", vec![]);
    if let ProfileKind::Ssh(s) = &mut key.kind {
        s.auth = AuthMethod::Key {
            path: None,
            passphrase: false,
        };
    }
    let store = ProfileStore {
        profiles: vec![key],
    };
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("path"), "no path is written: {text}");
    assert!(text.contains("type = \"key\""), "{text}");
    assert_eq!(ProfileStore::load(&path).unwrap(), store);
    // Written again, the file is byte for byte the same.
    store.save(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn old_key_profiles_load() {
    let path = tmp("profiles.toml");
    let old = "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\nkind = \"ssh\"\nhost = \"h\"\nport = 22\nuser = \"u\"\njump = []\n\n[profiles.auth]\ntype = \"key\"\npath = \"~/.ssh/id_ed25519\"\npassphrase = true\n";
    std::fs::write(&path, old).unwrap();
    let store = ProfileStore::load(&path).unwrap();
    let ProfileKind::Ssh(s) = &store.profiles[0].kind else {
        panic!("ssh")
    };
    assert_eq!(
        s.auth,
        AuthMethod::Key {
            path: Some("~/.ssh/id_ed25519".into()),
            passphrase: true,
        }
    );
    // Saved again, an old file keeps its path.
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("path = \"~/.ssh/id_ed25519\""), "{text}");
    assert_eq!(ProfileStore::load(&path).unwrap(), store);
    // A key profile written by hand without a path loads with none.
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\nkind = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nauth = { type = \"key\" }\n",
    )
    .unwrap();
    let store = ProfileStore::load(&path).unwrap();
    let ProfileKind::Ssh(s) = &store.profiles[0].kind else {
        panic!("ssh")
    };
    assert_eq!(
        s.auth,
        AuthMethod::Key {
            path: None,
            passphrase: false,
        }
    );
}

#[test]
fn accent_style_defaults_to_stripe_and_is_written_only_when_set() {
    let path = tmp("profiles.toml");
    let mut tinted = local("tinted");
    tinted.accent_style = AccentStyle::Tab;
    let store = ProfileStore {
        profiles: vec![local("plain"), tinted],
    };
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("accent_style").count(), 1, "{text}");
    assert!(text.contains("accent_style = \"tab\""));
    assert_eq!(ProfileStore::load(&path).unwrap(), store);

    // Files written before the setting existed load with the stripe.
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\naccent = \"#ed4a49\"\nkind = \"local\"\nprogram = \"/bin/sh\"\n",
    )
    .unwrap();
    let old = ProfileStore::load(&path).unwrap();
    assert_eq!(old.profiles[0].accent_style, AccentStyle::Stripe);
    // An unknown style is an error, not silently the stripe.
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\naccent_style = \"border\"\nkind = \"local\"\nprogram = \"/bin/sh\"\n",
    )
    .unwrap();
    assert!(ProfileStore::load(&path).is_err());
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
    // `check_updates` left out means "never asked"; written out, it is the answer "no".
    let answered = Settings {
        check_updates: Some(false),
        ..Settings::default()
    };
    assert_eq!(loaded, answered);
    assert!(warning.is_none());
    // Every field of Settings is written out.
    let table: toml::Table = toml::from_str(&uncommented).unwrap();
    let defaults: toml::Table = toml::from_str(&toml::to_string(&answered).unwrap()).unwrap();
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

#[test]
fn color_scheme_defaults_to_graphite() {
    assert_eq!(Settings::default().color_scheme, "graphite");
    let path = tmp("settings.toml");
    std::fs::write(&path, "").unwrap();
    assert_eq!(Settings::load(&path).unwrap().color_scheme, "graphite");
}

#[test]
fn color_scheme_write_field_round_trips() {
    let path = tmp("settings.toml");
    Settings::write_field(&path, &SettingChange::ColorScheme("harbor".into())).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\ncolor_scheme = \"harbor\"\n"), "{text}");
    assert!(
        text.contains("# Terminal color scheme for every session"),
        "{text}"
    );
    assert!(text.contains("# Color theme:"), "comments kept");
    assert_eq!(Settings::load(&path).unwrap().color_scheme, "harbor");
}

#[test]
fn profile_color_scheme_round_trips_and_is_omitted_when_none() {
    let path = tmp("profiles.toml");
    let mut themed = local("themed");
    themed.color_scheme = Some("ember".into());
    let store = ProfileStore {
        profiles: vec![local("plain"), themed],
    };
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("color_scheme").count(), 1, "{text}");
    assert!(text.contains("color_scheme = \"ember\""));
    assert_eq!(ProfileStore::load(&path).unwrap(), store);
}

#[test]
fn old_profiles_file_loads_without_color_scheme() {
    let path = tmp("profiles.toml");
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\nkind = \"local\"\nprogram = \"/bin/sh\"\n",
    )
    .unwrap();
    assert_eq!(
        ProfileStore::load(&path).unwrap().profiles[0].color_scheme,
        None
    );
}

fn ssh_mut(p: &mut Profile) -> &mut SshProfile {
    match &mut p.kind {
        ProfileKind::Ssh(s) => s,
        _ => panic!("expected ssh"),
    }
}

fn local_tunnel(name: &str, port: u16) -> TunnelDef {
    TunnelDef {
        name: name.into(),
        kind: TunnelKind::Local {
            bind: "127.0.0.1".into(),
            port,
            host: "db.internal".into(),
            host_port: 5432,
        },
        start: StartMode::OnDemand,
    }
}

#[test]
fn tunnels_round_trip_and_are_omitted_when_empty() {
    let path = tmp("profiles.toml");
    let mut with = ssh("with", vec![]);
    ssh_mut(&mut with).tunnels = vec![
        local_tunnel("db", 5432),
        TunnelDef {
            name: "web".into(),
            kind: TunnelKind::Remote {
                bind: "0.0.0.0".into(),
                port: 8080,
                host: "localhost".into(),
                host_port: 3000,
            },
            start: StartMode::WithShell,
        },
        TunnelDef {
            name: "proxy".into(),
            kind: TunnelKind::Socks {
                bind: "127.0.0.1".into(),
                port: 1080,
            },
            start: StartMode::OnDemand,
        },
    ];
    let store = ProfileStore {
        profiles: vec![ssh("without", vec![]), with],
    };
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("tunnels").count(), 3, "{text}");
    assert_eq!(text.matches("start = ").count(), 1, "{text}");
    assert!(text.contains("start = \"with_shell\""), "{text}");
    assert_eq!(ProfileStore::load(&path).unwrap(), store);
}

#[test]
fn tunnel_defaults_bind_loopback_and_on_demand() {
    let path = tmp("profiles.toml");
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\nkind = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nauth = { type = \"agent\" }\ntunnels = [\n  { name = \"db\", kind = \"local\", port = 5432, host = \"db.internal\", host_port = 5432 },\n  { name = \"s\", kind = \"socks\", port = 1080 },\n]\n",
    )
    .unwrap();
    let store = ProfileStore::load(&path).unwrap();
    let ProfileKind::Ssh(s) = &store.profiles[0].kind else {
        panic!("expected ssh")
    };
    assert_eq!(s.tunnels[0], local_tunnel("db", 5432));
    assert_eq!(
        s.tunnels[1].kind,
        TunnelKind::Socks {
            bind: "127.0.0.1".into(),
            port: 1080
        }
    );
    assert_eq!(s.tunnels[1].start, StartMode::OnDemand);
    assert!(!s.tunnels[0].non_loopback());
}

#[test]
fn old_profiles_load_without_tunnels() {
    let path = tmp("profiles.toml");
    std::fs::write(
        &path,
        "[[profiles]]\nid = \"67e55044-10b1-426f-9247-bb680e5fe0c8\"\nname = \"a\"\nkind = \"ssh\"\nhost = \"h\"\nuser = \"u\"\nauth = { type = \"agent\" }\n",
    )
    .unwrap();
    let store = ProfileStore::load(&path).unwrap();
    let ProfileKind::Ssh(s) = &store.profiles[0].kind else {
        panic!("expected ssh")
    };
    assert!(s.tunnels.is_empty());
}

#[test]
fn duplicate_tunnel_name_is_invalid() {
    let mut a = ssh("a", vec![]);
    ssh_mut(&mut a).tunnels = vec![local_tunnel("db", 1), local_tunnel("db", 2)];
    let errs = ProfileStore { profiles: vec![a] }.validate();
    assert_eq!(
        errs,
        vec![ValidationError::DuplicateTunnel {
            profile: "a".into(),
            name: "db".into()
        }]
    );
}

#[test]
fn empty_tunnel_name_is_invalid() {
    let mut a = ssh("a", vec![]);
    ssh_mut(&mut a).tunnels = vec![local_tunnel("  ", 1)];
    let errs = ProfileStore { profiles: vec![a] }.validate();
    assert_eq!(
        errs,
        vec![ValidationError::EmptyTunnelName {
            profile: "a".into()
        }]
    );
}

#[test]
fn local_port_zero_is_invalid() {
    let mut a = ssh("a", vec![]);
    ssh_mut(&mut a).tunnels = vec![
        local_tunnel("db", 0),
        TunnelDef {
            name: "proxy".into(),
            kind: TunnelKind::Socks {
                bind: "127.0.0.1".into(),
                port: 0,
            },
            start: StartMode::OnDemand,
        },
        TunnelDef {
            name: "rev".into(),
            kind: TunnelKind::Remote {
                bind: "127.0.0.1".into(),
                port: 0,
                host: "localhost".into(),
                host_port: 80,
            },
            start: StartMode::OnDemand,
        },
    ];
    let errs = ProfileStore { profiles: vec![a] }.validate();
    assert_eq!(
        errs,
        vec![
            ValidationError::TunnelPort {
                profile: "a".into(),
                name: "db".into()
            },
            ValidationError::TunnelPort {
                profile: "a".into(),
                name: "proxy".into()
            },
        ]
    );
}

#[test]
fn tunnel_max_retries_defaults_clamps_and_writes() {
    assert_eq!(Settings::default().tunnel_max_retries, 10);
    let path = tmp("settings.toml");
    std::fs::write(&path, "tunnel_max_retries = 500\n").unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.tunnel_max_retries, 100);
    assert!(warning.unwrap().message.contains("tunnel_max_retries"));
    std::fs::write(&path, "tunnel_max_retries = 0\n").unwrap();
    let (s, warning) = Settings::load_checked(&path).unwrap();
    assert_eq!(s.tunnel_max_retries, 0);
    assert!(warning.is_none());
    Settings::write_field(&path, &SettingChange::TunnelMaxRetries(7)).unwrap();
    assert_eq!(Settings::load(&path).unwrap().tunnel_max_retries, 7);
    assert!(SETTINGS_TEMPLATE.contains("#tunnel_max_retries = 10"));
}

#[test]
fn blank_target_port_blocks_save() {
    use fbterm_config::{StartMode, TunnelDef, TunnelKind, ValidationError};
    let mut store = ProfileStore::default();
    let mut p = Profile {
        icon: None,
        id: ProfileId::new(),
        name: "web".into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: AuthMethod::Agent,
            jump: vec![],
            tunnels: vec![TunnelDef {
                name: "db".into(),
                kind: TunnelKind::Local {
                    bind: "127.0.0.1".into(),
                    port: 5432,
                    host: "db".into(),
                    host_port: 0,
                },
                start: StartMode::OnDemand,
            }],
        }),
    };
    store.profiles.push(p.clone());
    assert_eq!(
        store.validate(),
        vec![ValidationError::TunnelTargetPort {
            profile: "web".into(),
            name: "db".into()
        }]
    );
    if let ProfileKind::Ssh(s) = &mut p.kind
        && let TunnelKind::Local { host_port, .. } = &mut s.tunnels[0].kind
    {
        *host_port = 5432;
    }
    store.profiles[0] = p;
    assert!(store.validate().is_empty());
}

#[test]
fn cycle_errors_name_each_profile_that_reaches_the_cycle() {
    let mut a = ssh("a", vec![]);
    let b = ssh("b", vec![a.id]);
    if let ProfileKind::Ssh(s) = &mut a.kind {
        s.jump = vec![b.id];
    }
    let c = ssh("c", vec![a.id]);
    let mut selfish = ssh("self", vec![]);
    if let ProfileKind::Ssh(s) = &mut selfish.kind {
        s.jump = vec![selfish.id];
    }
    let fine = ssh("fine", vec![]);
    let store = ProfileStore {
        profiles: vec![c, a, b, selfish, fine],
    };
    let cycle = |n: &str| ValidationError::JumpCycle {
        profile: n.to_string(),
    };
    assert_eq!(
        store.validate(),
        vec![cycle("c"), cycle("a"), cycle("b"), cycle("self")]
    );
    assert_eq!(cycle("a").to_string(), "profile \"a\" has a jump cycle");
}

#[test]
fn validate_is_linear_for_diamonds_and_long_chains() {
    use std::time::{Duration, Instant};
    // Each level jumps through the previous one twice: a naive walk takes 2^40 steps.
    let mut diamond = vec![ssh("d0", vec![])];
    for i in 1..=40 {
        let prev = diamond[i - 1].id;
        diamond.push(ssh(&format!("d{i}"), vec![prev, prev]));
    }
    // A chain far deeper than the stack allows recursion for, ending in a cycle.
    let mut chain = vec![ssh("c0", vec![])];
    for i in 1..10_000 {
        let prev = chain[i - 1].id;
        chain.push(ssh(&format!("c{i}"), vec![prev]));
    }
    let last = chain[9_999].id;
    if let ProfileKind::Ssh(s) = &mut chain[0].kind {
        s.jump = vec![last];
    }
    for (profiles, cycles) in [(diamond, 0), (chain, 10_000)] {
        let store = ProfileStore { profiles };
        let start = Instant::now();
        let errors = store.validate();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        let found = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::JumpCycle { .. }))
            .count();
        assert_eq!(found, cycles);
    }
}

/// Forty levels of two profiles, each jumping through both profiles of the level below: the
/// expanded chain doubles per level.
fn two_wide_diamond(levels: usize) -> Vec<Profile> {
    let mut out: Vec<Profile> = vec![ssh("a0", vec![]), ssh("b0", vec![])];
    for i in 1..levels {
        let below = vec![out[2 * i - 2].id, out[2 * i - 1].id];
        out.push(ssh(&format!("a{i}"), below.clone()));
        out.push(ssh(&format!("b{i}"), below));
    }
    out
}

#[test]
fn chains_longer_than_32_hops_are_refused() {
    use std::time::{Duration, Instant};
    // 32 hops are fine, 33 are not.
    let mut chain = vec![ssh("c0", vec![])];
    for i in 1..=33 {
        let prev = chain[i - 1].id;
        chain.push(ssh(&format!("c{i}"), vec![prev]));
    }
    let store = ProfileStore {
        profiles: chain.clone(),
    };
    assert_eq!(store.jump_chain(chain[32].id).unwrap().len(), 32);
    assert_eq!(
        store.validate(),
        vec![ValidationError::JumpChainTooLong {
            profile: "c33".into()
        }]
    );
    assert_eq!(
        store.jump_chain(chain[33].id),
        Err(ValidationError::JumpChainTooLong {
            profile: "c33".into()
        })
    );
    assert_eq!(
        ValidationError::JumpChainTooLong {
            profile: "c33".into()
        }
        .to_string(),
        "profile \"c33\" goes through more than 32 jump hosts"
    );

    // A diamond: few profiles, but an exponentially long expanded chain.
    let diamond = two_wide_diamond(40);
    let top = diamond[79].id;
    let store = ProfileStore { profiles: diamond };
    let start = Instant::now();
    let errors = store.validate();
    // Levels 0 to 4 expand to 0, 2, 6, 14 and 30 hops; every level above is too long.
    assert_eq!(errors.len(), 70, "{errors:?}");
    assert!(
        errors
            .iter()
            .all(|e| matches!(e, ValidationError::JumpChainTooLong { .. }))
    );
    assert!(matches!(
        store.jump_chain(top),
        Err(ValidationError::JumpChainTooLong { .. })
    ));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );

    // A chain far deeper than recursion allows never overflows.
    let mut deep = vec![ssh("d0", vec![])];
    for i in 1..50_000 {
        let prev = deep[i - 1].id;
        deep.push(ssh(&format!("d{i}"), vec![prev]));
    }
    let last = deep[49_999].id;
    let store = ProfileStore { profiles: deep };
    let start = Instant::now();
    assert!(matches!(
        store.jump_chain(last),
        Err(ValidationError::JumpChainTooLong { .. })
    ));
    assert_eq!(store.validate().len(), 49_999 - 32);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn duplicate_copies_every_field_but_id_and_name_right_after_the_original() {
    let mut full = ssh("db", vec![]);
    let hop = ssh("bastion", vec![]);
    full.group = Some("prod".into());
    full.accent = Some(Rgb { r: 1, g: 2, b: 3 });
    full.accent_style = AccentStyle::Tab;
    full.color_scheme = Some("solar".into());
    if let ProfileKind::Ssh(s) = &mut full.kind {
        s.port = 2222;
        s.auth = AuthMethod::Key {
            path: Some("/k/id".into()),
            passphrase: true,
        };
        s.jump = vec![hop.id];
        s.tunnels = vec![TunnelDef {
            name: "web".into(),
            kind: TunnelKind::Local {
                bind: "127.0.0.1".into(),
                port: 8080,
                host: "localhost".into(),
                host_port: 80,
            },
            start: StartMode::default(),
        }];
    }
    let mut shell = local("zsh");
    if let ProfileKind::Local(l) = &mut shell.kind {
        l.cwd = Some("/tmp".into());
        l.env.insert("A".into(), "1".into());
    }
    let mut store = ProfileStore {
        profiles: vec![hop.clone(), full.clone(), shell.clone()],
    };

    let id = store.duplicate(full.id).expect("a copy");
    assert_ne!(id, full.id);
    let names: Vec<&str> = store.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["bastion", "db", "db copy", "zsh"]);
    let copy = store.get(id).unwrap();
    assert_eq!(
        Profile {
            id: full.id,
            name: full.name.clone(),
            ..copy.clone()
        },
        full
    );

    let local_id = store.duplicate(shell.id).unwrap();
    let copy = store.get(local_id).unwrap();
    assert_eq!(copy.name, "zsh copy");
    assert_eq!(copy.kind, shell.kind);
    assert_eq!(
        (copy.group.clone(), copy.accent),
        (shell.group, shell.accent)
    );
    assert!(store.validate().is_empty());
}

#[test]
fn duplicate_names_count_up_until_unique() {
    let a = ssh("a", vec![]);
    let mut store = ProfileStore {
        profiles: vec![a.clone(), ssh("a copy", vec![]), ssh("a copy 3", vec![])],
    };
    let first = store.duplicate(a.id).unwrap();
    assert_eq!(store.get(first).unwrap().name, "a copy 2");
    let second = store.duplicate(a.id).unwrap();
    assert_eq!(store.get(second).unwrap().name, "a copy 4");
    // Newest copy first, right after the original.
    let names: Vec<&str> = store.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["a", "a copy 4", "a copy 2", "a copy", "a copy 3"]);
    // A copy of a copy.
    let third = store.duplicate(first).unwrap();
    assert_eq!(store.get(third).unwrap().name, "a copy 2 copy");
    assert!(store.validate().is_empty());
}

#[test]
fn duplicate_of_an_unknown_profile_changes_nothing() {
    let mut store = ProfileStore {
        profiles: vec![ssh("a", vec![])],
    };
    let before = store.clone();
    assert_eq!(store.duplicate(ProfileId::new()), None);
    assert_eq!(store, before);
}

#[test]
fn duplicate_profile_action_is_named_and_unbound_by_default() {
    use fbterm_config::keys::{Action, default_chord};
    assert_eq!(
        Action::from_name("duplicate_profile"),
        Some(Action::DuplicateProfile)
    );
    assert!(Action::ALL.contains(&Action::DuplicateProfile));
    assert_eq!(Action::DuplicateProfile.title(), "Duplicate profile");
    assert_eq!(default_chord(true, Action::DuplicateProfile), None);
    assert_eq!(default_chord(false, Action::DuplicateProfile), None);
}

#[test]
fn check_updates_is_unasked_until_written() {
    assert_eq!(Settings::default().check_updates, None);
    assert!(!Settings::default().checks_updates());
    let path = tmp("settings.toml");
    std::fs::write(&path, "font_size = 14.0\n").unwrap();
    assert_eq!(Settings::load(&path).unwrap().check_updates, None);
    Settings::write_field(&path, &SettingChange::CheckUpdates(true)).unwrap();
    let s = Settings::load(&path).unwrap();
    assert_eq!(s.check_updates, Some(true));
    assert!(s.checks_updates());
    Settings::write_field(&path, &SettingChange::CheckUpdates(false)).unwrap();
    assert_eq!(Settings::load(&path).unwrap().check_updates, Some(false));
    assert!(SETTINGS_TEMPLATE.contains("#check_updates = false"));
    // Never asked: nothing about it is saved.
    assert!(
        !toml::to_string(&Settings::default())
            .unwrap()
            .contains("check_updates")
    );
}

#[test]
fn icon_is_written_only_when_set() {
    let path = tmp("profiles.toml");
    let mut db = local("db");
    db.icon = Some("database".into());
    let store = ProfileStore {
        profiles: vec![local("plain"), db],
    };
    store.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("icon").count(), 1, "{text}");
    assert_eq!(ProfileStore::load(&path).unwrap(), store);
}
