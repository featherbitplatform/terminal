//! The profiles example published on the docs site must stay valid.

use std::path::Path;

use fbterm_config::*;

fn example() -> ProfileStore {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../website/static/examples/profiles.toml");
    ProfileStore::load(&path).expect("docs example parses")
}

#[test]
fn docs_profiles_example_loads_and_validates() {
    let store = example();
    assert_eq!(store.profiles.len(), 4);
    assert_eq!(store.validate(), vec![]);
}

#[test]
fn docs_profiles_example_shows_each_feature() {
    let store = example();
    let by_name = |n: &str| store.profiles.iter().find(|p| p.name == n).unwrap();

    let ProfileKind::Local(local) = &by_name("Local zsh").kind else {
        panic!("local")
    };
    assert_eq!(local.args, vec!["-l"]);

    let ProfileKind::Ssh(bastion) = &by_name("bastion").kind else {
        panic!("ssh")
    };
    assert_eq!(bastion.port, 22, "port defaults to 22");
    assert!(matches!(
        bastion.auth,
        AuthMethod::Key {
            passphrase: true,
            ..
        }
    ));
    assert_eq!(by_name("bastion").accent.unwrap().to_string(), "#e0a030");
    assert_eq!(by_name("bastion").accent_style, AccentStyle::Tab);
    assert_eq!(by_name("api-1").accent_style, AccentStyle::Stripe);

    let api = by_name("api-1");
    let ProfileKind::Ssh(ssh) = &api.kind else {
        panic!("ssh")
    };
    assert_eq!(ssh.port, 2222);
    assert_eq!(ssh.auth, AuthMethod::Agent);

    let db = by_name("db-1");
    let chain: Vec<_> = store
        .jump_chain(db.id)
        .unwrap()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(chain, ["bastion", "api-1"]);
}
