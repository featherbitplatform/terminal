use std::path::PathBuf;

use fbterm_config::schemes::{
    BUILTIN_IDS, Scheme, SchemeSet, builtins, contrast_ok, delete_user, new_user_id, save_user,
};
use fbterm_config::{Paths, ProfileId, Rgb};

fn tmp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fbterm-schemes-test-{}", ProfileId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn user_scheme(id: &str, name: &str) -> Scheme {
    let mut s = builtins().remove(2);
    s.id = id.to_string();
    s.name = name.to_string();
    s.builtin = false;
    s
}

#[test]
fn builtins_have_fixed_ids_and_names() {
    let all = builtins();
    let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, BUILTIN_IDS);
    let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Graphite", "Graphite Light", "Harbor", "Ember"]);
    assert!(all.iter().all(|s| s.builtin));
    assert!(
        all.iter()
            .all(|s| s.font_family.is_none() && s.font_size.is_none())
    );
}

#[test]
fn graphite_builtin_matches_today() {
    let g = &builtins()[0].colors;
    let hex = |s: &str| s.parse::<Rgb>().unwrap();
    assert_eq!(g.foreground, hex("#f5f5f7"));
    assert_eq!(g.background, hex("#0b0b0e"));
    assert_eq!(g.cursor, hex("#a584ff"));
    assert_eq!(g.selection, hex("#392f56"));
    assert_eq!(g.ansi[0], hex("#262629"));
    assert_eq!(g.ansi[7], hex("#d0d0d9"));
    assert_eq!(g.ansi[8], hex("#919199"));
    assert_eq!(g.ansi[15], hex("#f5f5f7"));
}

#[test]
fn builtins_meet_the_contrast_bar() {
    for s in builtins() {
        assert!(contrast_ok(&s.colors), "{} misses the contrast bar", s.id);
    }
    let mut bad = builtins().remove(0).colors;
    bad.ansi[1] = bad.background;
    assert!(!contrast_ok(&bad));
}

#[test]
fn user_scheme_round_trips() {
    let dir = tmp_dir();
    let id = new_user_id();
    assert!(id.starts_with("user-"));
    let mut s = user_scheme(&id, "Night Shift");
    s.font_family = Some("Geist Mono".into());
    s.font_size = Some(13.5);
    let path = save_user(&dir, &s).unwrap();
    assert_eq!(path, dir.join(format!("{id}.toml")));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[colors]") && text.contains("bright_white"));
    assert!(!text.contains("builtin"));
    let (set, errors) = SchemeSet::load(&dir);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(set.all().len(), 5);
    assert_eq!(set.get(&id), Some(&s));
    assert!(!set.get(&id).unwrap().builtin);
    assert!(set.get("graphite").unwrap().builtin);
}

#[test]
fn load_from_missing_dir_gives_builtins() {
    let (set, errors) = SchemeSet::load(&tmp_dir().join("nope"));
    assert!(errors.is_empty());
    assert_eq!(set.all().len(), 4);
}

#[test]
fn bad_scheme_file_is_skipped_with_error() {
    let dir = tmp_dir();
    save_user(&dir, &user_scheme("user-good", "Good")).unwrap();
    let bad = save_user(&dir, &user_scheme("user-bad", "Bad")).unwrap();
    let text = std::fs::read_to_string(&bad).unwrap();
    let broken: String = text
        .lines()
        .map(|l| {
            if l.starts_with("background") {
                "background = \"red\"".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&bad, &broken).unwrap();
    let line = broken
        .lines()
        .position(|l| l.starts_with("background"))
        .unwrap()
        + 1;
    let (set, errors) = SchemeSet::load(&dir);
    assert!(set.get("user-good").is_some());
    assert!(set.get("user-bad").is_none());
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, bad);
    assert_eq!(errors[0].line, Some(line));
}

#[test]
fn builtin_id_in_user_file_is_skipped() {
    let dir = tmp_dir();
    let mut s = user_scheme("x", "Fake");
    s.id = "harbor".into();
    fbterm_config::schemes::write_file(&dir.join("fake.toml"), &s).unwrap();
    let (set, errors) = SchemeSet::load(&dir);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, dir.join("fake.toml"));
    assert_eq!(set.all().len(), 4);
    assert_eq!(set.get("harbor").unwrap().name, "Harbor");
}

#[test]
fn duplicate_user_id_is_skipped() {
    let dir = tmp_dir();
    let s = user_scheme("user-dup", "First");
    fbterm_config::schemes::write_file(&dir.join("a.toml"), &s).unwrap();
    let mut t = s.clone();
    t.name = "Second".into();
    fbterm_config::schemes::write_file(&dir.join("b.toml"), &t).unwrap();
    let (set, errors) = SchemeSet::load(&dir);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, dir.join("b.toml"));
    assert_eq!(set.get("user-dup").unwrap().name, "First");
    assert_eq!(set.all().len(), 5);
}

#[test]
fn save_refuses_builtins() {
    let dir = tmp_dir();
    let mut s = builtins().remove(1);
    assert!(save_user(&dir, &s).is_err());
    s.builtin = false;
    assert!(save_user(&dir, &s).is_err(), "built-in id is refused too");
    assert!(save_user(&dir, &user_scheme("../evil", "Evil")).is_err());
    assert!(!dir.join("graphite-light.toml").exists());
}

#[test]
fn delete_removes_the_file() {
    let dir = tmp_dir();
    let path = save_user(&dir, &user_scheme("user-gone", "Gone")).unwrap();
    assert!(path.exists());
    delete_user(&dir, "user-gone").unwrap();
    assert!(!path.exists());
    assert!(delete_user(&dir, "graphite").is_err());
}

#[test]
fn schemes_dir_is_under_config_dir() {
    let paths = Paths {
        config_dir: PathBuf::from("cfg"),
    };
    assert_eq!(paths.schemes(), PathBuf::from("cfg").join("schemes"));
}
