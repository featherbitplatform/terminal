//! Editor state rules.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

use fbterm_secrets::{MemoryStore, SecretError};
use parking_lot::Mutex;

/// Records every call, backed by memory; `set` can be made to fail.
#[derive(Default)]
struct Recording {
    inner: MemoryStore,
    removed: Mutex<Vec<ProfileId>>,
    removed_kinds: Mutex<Vec<(ProfileId, SecretKind)>>,
    /// Every call in order: "set", "remove_kind", "remove".
    log: Mutex<Vec<&'static str>>,
    fail_set: AtomicBool,
}

impl SecretStore for Recording {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        self.inner.get(id, kind)
    }
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        self.log.lock().push("set");
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(SecretError::Backend("denied".into()));
        }
        self.inner.set(id, kind, value)
    }
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        self.log.lock().push("remove");
        self.removed.lock().push(*id);
        self.inner.remove(id)
    }
    fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
        self.log.lock().push("remove_kind");
        self.removed_kinds.lock().push((*id, kind));
        self.inner.remove_kind(id, kind)
    }
}

/// A fresh, writable `profiles.toml` path.
fn file() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fbterm-editor-{}", ProfileId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("profiles.toml")
}

/// A `profiles.toml` path that cannot be written (its parent is a file).
fn unwritable() -> PathBuf {
    let blocker = std::env::temp_dir().join(format!("fbterm-blocker-{}", ProfileId::new()));
    std::fs::write(&blocker, "not a directory").unwrap();
    blocker.join("profiles.toml")
}

fn ssh_with(name: &str, auth: AuthMethod) -> Profile {
    let mut p = ssh(name, "10.0.0.5", vec![]);
    if let ProfileKind::Ssh(s) = &mut p.kind {
        s.auth = auth;
    }
    p
}

fn typed_secret(ed: &EditorState, id: ProfileId) -> String {
    match &ed.draft(id).unwrap().kind {
        DraftKind::Ssh(s) => s.secret.clone(),
        DraftKind::Local(_) => panic!("ssh"),
    }
}

fn ssh(name: &str, host: &str, jump: Vec<ProfileId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: host.into(),
            port: 22,
            user: "fb".into(),
            auth: AuthMethod::Agent,
            jump,
            tunnels: Vec::new(),
        }),
    }
}

fn local(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
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

fn editor(store: &ProfileStore) -> (EditorState, Arc<Recording>) {
    let secrets = Arc::new(Recording::default());
    (EditorState::new(store, secrets.clone(), None), secrets)
}

#[test]
fn new_ssh_profile_with_empty_host_is_refused() {
    let mut store = ProfileStore {
        profiles: vec![local("zsh")],
    };
    let before = store.clone();
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::NewSsh);
    let name = ed.selected_draft().unwrap().name.clone();
    let path = file();
    let Err(SaveError::Invalid(errs)) = ed.save(&mut store, &path) else {
        panic!("refused")
    };
    assert_eq!(errs, vec![ValidationError::EmptyHost(name)]);
    assert_eq!(ed.errors, errs);
    assert_eq!(store, before, "an invalid store is never applied");
    assert!(!path.exists(), "an invalid store is never written");

    ed.update(EditorMsg::Host("10.0.0.5".into()));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(store.profiles.len(), 2);
    assert!(ed.errors.is_empty());
}

#[test]
fn jump_list_offers_only_other_ssh_profiles() {
    let a = ssh("a", "a.example", vec![]);
    let b = ssh("b", "b.example", vec![]);
    let c = ssh("c", "c.example", vec![]);
    let shell = local("zsh");
    let store = ProfileStore {
        profiles: vec![a.clone(), b.clone(), shell, c.clone()],
    };
    let (mut ed, _) = editor(&store);
    let names = |ed: &EditorState| -> Vec<String> {
        ed.jump_options(a.id).into_iter().map(|(_, n)| n).collect()
    };
    assert_eq!(names(&ed), ["b", "c"]);
    ed.update(EditorMsg::Select(a.id));
    ed.update(EditorMsg::JumpAdd(c.id));
    assert_eq!(
        names(&ed),
        ["b"],
        "hops already in the chain are not offered"
    );
    ed.update(EditorMsg::JumpAdd(a.id));
    ed.update(EditorMsg::JumpAdd(b.id));
    let DraftKind::Ssh(s) = &ed.draft(a.id).unwrap().kind else {
        panic!("ssh")
    };
    assert_eq!(
        s.jump,
        vec![c.id, b.id],
        "a profile never jumps through itself"
    );
    ed.update(EditorMsg::JumpUp(1));
    let DraftKind::Ssh(s) = &ed.draft(a.id).unwrap().kind else {
        panic!("ssh")
    };
    assert_eq!(s.jump, vec![b.id, c.id]);
}

#[test]
fn renaming_keeps_the_id_so_jumps_survive() {
    let bastion = ssh("bastion", "b.example", vec![]);
    let web = ssh("web", "10.0.0.5", vec![bastion.id]);
    let mut store = ProfileStore {
        profiles: vec![bastion.clone(), web.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Select(bastion.id));
    ed.update(EditorMsg::Name("edge".into()));
    ed.save(&mut store, &file()).unwrap();
    let renamed = store.get(bastion.id).expect("same id");
    assert_eq!(renamed.name, "edge");
    let chain: Vec<&str> = store
        .jump_chain(web.id)
        .unwrap()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(chain, ["edge"]);
}

#[test]
fn deleting_a_jump_host_is_refused_with_its_dependants() {
    let bastion = ssh("bastion", "b.example", vec![]);
    let web = ssh("web", "10.0.0.5", vec![bastion.id]);
    let db = ssh("db", "10.0.0.6", vec![bastion.id]);
    let mut store = ProfileStore {
        profiles: vec![bastion.clone(), web, db],
    };
    let (mut ed, secrets) = editor(&store);
    assert_eq!(
        ed.delete(bastion.id),
        Err(vec!["web".to_owned(), "db".to_owned()])
    );
    ed.update(EditorMsg::Select(bastion.id));
    ed.update(EditorMsg::Delete);
    assert!(ed.draft(bastion.id).is_some());
    assert!(ed.notice.as_deref().unwrap().contains("web, db"));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(store.profiles.len(), 3);
    assert!(secrets.removed.lock().is_empty());
}

#[test]
fn typed_password_goes_to_the_secret_store_never_the_profile() {
    let web = ssh("web", "10.0.0.5", vec![]);
    let mut store = ProfileStore {
        profiles: vec![web.clone()],
    };
    let (mut ed, secrets) = editor(&store);
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Auth(AuthChoice::Password));
    ed.update(EditorMsg::Secret(Redacted("hunter2-secret".into())));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(
        secrets
            .get(&web.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("hunter2-secret")
    );
    let ProfileKind::Ssh(s) = &store.get(web.id).unwrap().kind else {
        panic!("ssh")
    };
    assert_eq!(s.auth, AuthMethod::Password);
    let file = toml::to_string_pretty(&store).unwrap();
    assert!(
        !file.contains("hunter2"),
        "secret leaked into the file: {file}"
    );
    let DraftKind::Ssh(d) = &ed.draft(web.id).unwrap().kind else {
        panic!("ssh")
    };
    assert!(
        d.secret.is_empty(),
        "the typed secret is not kept after saving"
    );
}

#[test]
fn deleting_a_profile_removes_its_secrets() {
    let web = ssh("web", "10.0.0.5", vec![]);
    let shell = local("zsh");
    let mut store = ProfileStore {
        profiles: vec![shell, web.clone()],
    };
    let (mut ed, secrets) = editor(&store);
    secrets.set(&web.id, SecretKind::Password, "pw").unwrap();
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Delete);
    assert!(ed.draft(web.id).is_none());
    assert!(
        secrets.removed.lock().is_empty(),
        "nothing is removed before saving"
    );
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(*secrets.removed.lock(), vec![web.id]);
    assert_eq!(secrets.get(&web.id, SecretKind::Password).unwrap(), None);
    assert!(store.get(web.id).is_none());
}

#[test]
fn read_only_editor_ignores_edits() {
    let shell = local("zsh");
    let store = ProfileStore {
        profiles: vec![shell.clone()],
    };
    let err = ConfigError {
        path: "/tmp/profiles.toml".into(),
        line: Some(3),
        message: "expected `=`".into(),
    };
    let mut ed = EditorState::new(&store, Arc::new(MemoryStore::new()), Some(err));
    ed.update(EditorMsg::Name("other".into()));
    ed.update(EditorMsg::NewSsh);
    ed.update(EditorMsg::Delete);
    assert_eq!(ed.drafts().len(), 1);
    assert_eq!(ed.drafts()[0].name, "zsh");
    assert!(!ed.dirty);
}

#[test]
fn port_accepts_only_valid_numbers() {
    let web = ssh("web", "h", vec![]);
    let store = ProfileStore {
        profiles: vec![web.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Port("22x".into()));
    ed.update(EditorMsg::Port("70000".into()));
    let port = |ed: &EditorState| match &ed.draft(web.id).unwrap().kind {
        DraftKind::Ssh(s) => s.port.clone(),
        DraftKind::Local(_) => unreachable!(),
    };
    assert_eq!(port(&ed), "22");
    ed.update(EditorMsg::Port("2222".into()));
    assert_eq!(port(&ed), "2222");
}

#[test]
fn groups_sort_named_first_then_ungrouped() {
    let mut a = ssh("a", "h", vec![]);
    a.group = Some("prod".into());
    let mut b = ssh("b", "h", vec![]);
    b.group = Some("Lab".into());
    let c = local("zsh");
    let store = ProfileStore {
        profiles: vec![a, b, c],
    };
    let (ed, _) = editor(&store);
    let groups: Vec<(String, Vec<&str>)> = ed
        .groups()
        .into_iter()
        .map(|(g, ds)| (g, ds.iter().map(|d| d.name.as_str()).collect()))
        .collect();
    assert_eq!(
        groups,
        vec![
            ("Lab".to_owned(), vec!["b"]),
            ("prod".to_owned(), vec!["a"]),
            (String::new(), vec!["zsh"]),
        ]
    );
}

#[test]
fn failed_write_keeps_secrets_and_draft() {
    let web = ssh_with("web", AuthMethod::Password);
    let gone = ssh("gone", "10.0.0.9", vec![]);
    let mut store = ProfileStore {
        profiles: vec![web.clone(), gone.clone()],
    };
    let before = store.clone();
    let (mut ed, secrets) = editor(&store);
    ed.update(EditorMsg::Select(gone.id));
    ed.update(EditorMsg::Delete);
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Secret(Redacted("typed-pw".into())));

    let result = ed.save(&mut store, &unwritable());
    assert!(matches!(result, Err(SaveError::Write(_))), "{result:?}");
    assert_eq!(store, before, "the in-memory store is unchanged");
    assert!(secrets.log.lock().is_empty(), "no secret is touched");
    assert_eq!(typed_secret(&ed, web.id), "typed-pw");
    assert!(ed.dirty);

    // The same edits saved to a writable path go through, removals after the stores.
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(*secrets.log.lock(), ["set", "remove"]);
    assert_eq!(*secrets.removed.lock(), vec![gone.id]);
    assert_eq!(
        secrets
            .get(&web.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("typed-pw")
    );
    assert!(typed_secret(&ed, web.id).is_empty());
    assert!(!ed.dirty);
}

#[test]
fn removes_happen_only_after_a_successful_write() {
    let web = ssh_with("web", AuthMethod::Password);
    let mut store = ProfileStore {
        profiles: vec![web.clone(), local("zsh")],
    };
    let (mut ed, secrets) = editor(&store);
    secrets.set(&web.id, SecretKind::Password, "pw").unwrap();
    secrets.log.lock().clear();
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Delete);
    assert!(ed.save(&mut store, &unwritable()).is_err());
    assert!(secrets.removed.lock().is_empty());
    assert_eq!(
        secrets
            .get(&web.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("pw")
    );
    let path = file();
    ed.save(&mut store, &path).unwrap();
    assert!(path.exists());
    assert_eq!(*secrets.removed.lock(), vec![web.id]);
}

#[test]
fn refused_secret_keeps_the_draft_dirty() {
    let web = ssh_with("web", AuthMethod::Password);
    let mut store = ProfileStore {
        profiles: vec![web.clone()],
    };
    let (mut ed, secrets) = editor(&store);
    secrets.fail_set.store(true, Ordering::SeqCst);
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Secret(Redacted("typed-pw".into())));
    let Err(SaveError::Secrets(failures)) = ed.save(&mut store, &file()) else {
        panic!("the refused secret is reported")
    };
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("password of web"), "{failures:?}");
    assert!(!failures[0].contains("typed-pw"));
    assert_eq!(typed_secret(&ed, web.id), "typed-pw");
    assert!(ed.dirty);
    secrets.fail_set.store(false, Ordering::SeqCst);
    ed.save(&mut store, &file()).unwrap();
    assert!(typed_secret(&ed, web.id).is_empty());
}

#[test]
fn secrets_of_an_auth_no_longer_used_are_removed() {
    let web = ssh_with("web", AuthMethod::Password);
    let key = ssh_with(
        "key",
        AuthMethod::Key {
            path: "~/.ssh/id_ed25519".into(),
            passphrase: true,
        },
    );
    let agent = ssh_with("agent", AuthMethod::Agent);
    let mut store = ProfileStore {
        profiles: vec![web.clone(), key.clone(), agent.clone()],
    };
    let (mut ed, secrets) = editor(&store);
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Auth(AuthChoice::Agent));
    ed.update(EditorMsg::Select(key.id));
    ed.update(EditorMsg::KeyPassphrase(false));
    ed.update(EditorMsg::Select(agent.id));
    ed.update(EditorMsg::Auth(AuthChoice::Password));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(
        *secrets.removed_kinds.lock(),
        vec![
            (web.id, SecretKind::Password),
            (key.id, SecretKind::KeyPassphrase)
        ]
    );

    // Nothing changed: nothing removed again.
    secrets.removed_kinds.lock().clear();
    ed.update(EditorMsg::Select(web.id));
    ed.update(EditorMsg::Name("web2".into()));
    ed.save(&mut store, &file()).unwrap();
    assert!(secrets.removed_kinds.lock().is_empty());
}

#[test]
fn debug_output_never_shows_a_typed_secret() {
    let msg = EditorMsg::Secret(Redacted("hunter2-secret".into()));
    let app_msg = crate::app::Message::Editor(msg.clone());
    let op = SecretOp::Set {
        id: ProfileId::new(),
        kind: SecretKind::Password,
        value: Redacted("hunter2-secret".into()),
    };
    for shown in [
        format!("{msg:?}"),
        format!("{app_msg:?}"),
        format!("{op:?}"),
    ] {
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }
    let web = ssh_with("web", AuthMethod::Password);
    let (mut ed, _) = editor(&ProfileStore {
        profiles: vec![web.clone()],
    });
    ed.update(EditorMsg::Select(web.id));
    ed.update(msg);
    let draft = format!("{:?}", ed.draft(web.id).unwrap());
    assert!(!draft.contains("hunter2"), "{draft}");
}

#[test]
fn rsa_key_file_shows_a_warning_that_follows_the_path() {
    let dir = std::env::temp_dir().join(format!("fbterm-keys-{}", ProfileId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let rsa = dir.join("id_rsa");
    std::fs::write(
        &rsa,
        "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\n",
    )
    .unwrap();
    let other = dir.join("id_other");
    std::fs::write(&other, "not a key").unwrap();

    let p = ssh_with(
        "k",
        AuthMethod::Key {
            path: rsa.clone(),
            passphrase: false,
        },
    );
    let (mut ed, _) = editor(&ProfileStore { profiles: vec![p] });
    assert!(ed.selected_key_is_rsa());
    ed.update(EditorMsg::KeyPath(other.display().to_string()));
    assert!(!ed.selected_key_is_rsa());
    ed.update(EditorMsg::KeyPicked(Some(rsa)));
    assert!(ed.selected_key_is_rsa());
    ed.update(EditorMsg::Auth(AuthChoice::Agent));
    assert!(!ed.selected_key_is_rsa());
}

#[test]
fn rsa_warning_does_not_outlive_the_selected_profile() {
    let dir = std::env::temp_dir().join(format!("fbterm-keys-{}", ProfileId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let rsa = dir.join("id_rsa");
    std::fs::write(
        &rsa,
        "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\n",
    )
    .unwrap();
    let a = ssh_with(
        "a",
        AuthMethod::Key {
            path: rsa,
            passphrase: false,
        },
    );
    let a_id = a.id;
    let b = ssh_with("b", AuthMethod::Agent);
    let (mut ed, _) = editor(&ProfileStore {
        profiles: vec![a, b],
    });
    assert!(ed.selected_key_is_rsa());
    ed.update(EditorMsg::Delete);
    assert!(ed.draft(a_id).is_none());
    assert!(!ed.selected_key_is_rsa());
    ed.update(EditorMsg::NewSsh);
    assert!(!ed.selected_key_is_rsa());
}

#[test]
fn color_as_is_saved_with_the_profile_and_kept_untouched() {
    let shell = local("zsh");
    let other = local("other");
    let mut store = ProfileStore {
        profiles: vec![shell.clone(), other.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Select(shell.id));
    ed.update(EditorMsg::Accent(Some(Rgb {
        r: 0xed,
        g: 0x4a,
        b: 0x49,
    })));
    ed.update(EditorMsg::AccentStyle(AccentStyle::Tab));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(store.get(shell.id).unwrap().accent_style, AccentStyle::Tab);
    assert_eq!(
        store.get(other.id).unwrap().accent_style,
        AccentStyle::Stripe
    );
    // Reopened, the editor shows the saved style.
    let (ed, _) = editor(&store);
    assert_eq!(ed.draft(shell.id).unwrap().accent_style, AccentStyle::Tab);
}

#[test]
fn clearing_the_accent_resets_color_as_to_the_stripe() {
    let shell = local("zsh");
    let mut store = ProfileStore {
        profiles: vec![shell.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Select(shell.id));
    ed.update(EditorMsg::Accent(Some(Rgb {
        r: 0x2b,
        g: 0xbb,
        b: 0x71,
    })));
    ed.update(EditorMsg::AccentStyle(AccentStyle::Tab));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(store.get(shell.id).unwrap().accent_style, AccentStyle::Tab);
    ed.update(EditorMsg::Accent(None));
    let path = file();
    ed.save(&mut store, &path).unwrap();
    assert_eq!(
        store.get(shell.id).unwrap().accent_style,
        AccentStyle::Stripe
    );
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("accent_style")
    );
}

#[test]
fn editor_scheme_choice_saves_with_profile() {
    let shell = local("zsh");
    let mut store = ProfileStore {
        profiles: vec![shell.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Select(shell.id));
    ed.update(EditorMsg::ColorScheme(Some("ember".into())));
    let path = file();
    ed.save(&mut store, &path).unwrap();
    assert_eq!(
        store.get(shell.id).unwrap().color_scheme.as_deref(),
        Some("ember")
    );
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("color_scheme = \"ember\"")
    );
}

#[test]
fn editor_use_global_clears_override() {
    let mut shell = local("zsh");
    shell.color_scheme = Some("harbor".into());
    let mut store = ProfileStore {
        profiles: vec![shell.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Select(shell.id));
    ed.update(EditorMsg::ColorScheme(None));
    ed.save(&mut store, &file()).unwrap();
    assert_eq!(store.get(shell.id).unwrap().color_scheme, None);
}

#[test]
fn draft_keeps_color_scheme() {
    let mut p = ssh("web", "example.test", vec![]);
    p.color_scheme = Some("user-1234".into());
    let d = Draft::from_profile(&p);
    assert_eq!(d.color_scheme.as_deref(), Some("user-1234"));
    assert_eq!(d.to_profile().color_scheme.as_deref(), Some("user-1234"));
}

fn tunnels_of(ed: &EditorState, id: ProfileId) -> Vec<TunnelDef> {
    match &ed.draft(id).unwrap().kind {
        DraftKind::Ssh(s) => s.tunnels.clone(),
        DraftKind::Local(_) => unreachable!(),
    }
}

/// A one-profile editor with one fresh tunnel added.
fn with_tunnel() -> (EditorState, ProfileId, ProfileStore) {
    let web = ssh("web", "10.0.0.5", vec![]);
    let store = ProfileStore {
        profiles: vec![web.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::TunnelAdd);
    (ed, web.id, store)
}

#[test]
fn add_edit_and_save_tunnel() {
    let (mut ed, id, mut store) = with_tunnel();
    let t = tunnels_of(&ed, id);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].bind(), "127.0.0.1", "binds to loopback by default");
    assert_eq!(t[0].start, StartMode::OnDemand);
    ed.update(EditorMsg::TunnelName(0, "db".into()));
    ed.update(EditorMsg::TunnelPort(0, "5432".into()));
    ed.update(EditorMsg::TunnelHost(0, "db.internal".into()));
    ed.update(EditorMsg::TunnelHostPort(0, "5432".into()));
    ed.update(EditorMsg::TunnelStart(0, StartMode::WithShell));
    assert!(ed.dirty);
    ed.save(&mut store, &file()).unwrap();
    let ProfileKind::Ssh(s) = &store.profiles[0].kind else {
        unreachable!()
    };
    assert_eq!(
        s.tunnels,
        vec![TunnelDef {
            name: "db".into(),
            kind: TunnelKind::Local {
                bind: "127.0.0.1".into(),
                port: 5432,
                host: "db.internal".into(),
                host_port: 5432,
            },
            start: StartMode::WithShell,
        }]
    );

    // Switching the kind keeps the shared fields; SOCKS drops the target.
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Socks));
    let t = tunnels_of(&ed, id);
    assert_eq!(
        t[0].kind,
        TunnelKind::Socks {
            bind: "127.0.0.1".into(),
            port: 5432
        }
    );
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Remote));
    assert!(matches!(
        tunnels_of(&ed, id)[0].kind,
        TunnelKind::Remote { port: 5432, .. }
    ));
    ed.update(EditorMsg::TunnelRemove(0));
    assert!(tunnels_of(&ed, id).is_empty());
}

#[test]
fn tunnel_port_filters_input() {
    let (mut ed, id, _) = with_tunnel();
    let port = |ed: &EditorState| match &tunnels_of(ed, id)[0].kind {
        TunnelKind::Local { port, .. } => *port,
        _ => unreachable!(),
    };
    ed.update(EditorMsg::TunnelPort(0, "8080".into()));
    ed.update(EditorMsg::TunnelPort(0, "80x".into()));
    ed.update(EditorMsg::TunnelPort(0, "70000".into()));
    ed.update(EditorMsg::TunnelPort(0, "0".into()));
    assert_eq!(port(&ed), 8080, "bad input is ignored");
    ed.update(EditorMsg::TunnelPort(0, String::new()));
    assert_eq!(port(&ed), 0, "emptying the field clears it");
    ed.update(EditorMsg::TunnelHostPort(0, "5x".into()));
    assert!(matches!(
        tunnels_of(&ed, id)[0].kind,
        TunnelKind::Local { host_port: 0, .. }
    ));
    // A remote bind may ask the server to pick the port.
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Remote));
    ed.update(EditorMsg::TunnelPort(0, "0".into()));
    ed.update(EditorMsg::TunnelPort(0, "9".into()));
    ed.update(EditorMsg::TunnelPort(0, "0".into()));
    assert!(matches!(
        tunnels_of(&ed, id)[0].kind,
        TunnelKind::Remote { port: 0, .. }
    ));
}

#[test]
fn duplicate_tunnel_blocks_save() {
    let (mut ed, _, mut store) = with_tunnel();
    ed.update(EditorMsg::TunnelAdd);
    for i in 0..2 {
        ed.update(EditorMsg::TunnelName(i, "db".into()));
        ed.update(EditorMsg::TunnelPort(i, (5000 + i).to_string()));
        ed.update(EditorMsg::TunnelHost(i, "db.internal".into()));
        ed.update(EditorMsg::TunnelHostPort(i, "5432".into()));
    }
    let path = file();
    let Err(SaveError::Invalid(errs)) = ed.save(&mut store, &path) else {
        panic!("refused")
    };
    assert_eq!(
        errs,
        vec![ValidationError::DuplicateTunnel {
            profile: "web".into(),
            name: "db".into()
        }]
    );
    assert!(!path.exists());
    ed.update(EditorMsg::TunnelName(1, "cache".into()));
    ed.save(&mut store, &path).unwrap();
}

#[test]
fn non_loopback_shows_warning() {
    let (mut ed, id, _) = with_tunnel();
    let warning = |ed: &EditorState| {
        let DraftKind::Ssh(s) = &ed.draft(id).unwrap().kind else {
            unreachable!()
        };
        tunnel_warning(&s.tunnels[0])
    };
    assert_eq!(warning(&ed), None);
    ed.update(EditorMsg::TunnelBind(0, "0.0.0.0".into()));
    assert_eq!(
        warning(&ed).as_deref(),
        Some("Listens on 0.0.0.0: other machines can reach this tunnel.")
    );
    ed.update(EditorMsg::TunnelBind(0, "localhost".into()));
    assert_eq!(warning(&ed), None);
}

#[test]
fn empty_bind_or_host_blocks_save() {
    let (mut ed, _, mut store) = with_tunnel();
    ed.update(EditorMsg::TunnelName(0, "db".into()));
    ed.update(EditorMsg::TunnelPort(0, "5432".into()));
    ed.update(EditorMsg::TunnelHostPort(0, "5432".into()));
    ed.update(EditorMsg::TunnelBind(0, "  ".into()));
    let Err(SaveError::Invalid(errs)) = ed.save(&mut store, &file()) else {
        panic!("refused")
    };
    let p = || "web".to_owned();
    let n = || "db".to_owned();
    assert_eq!(
        errs,
        vec![
            ValidationError::EmptyTunnelBind {
                profile: p(),
                name: n()
            },
            ValidationError::EmptyTunnelHost {
                profile: p(),
                name: n()
            },
        ]
    );
    assert!(errs[0].to_string().contains("\"db\""));
    // SOCKS has no target host.
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Socks));
    ed.update(EditorMsg::TunnelBind(0, "127.0.0.1".into()));
    ed.save(&mut store, &file()).unwrap();
}

/// Fill a fresh local tunnel so only the field under test is wrong.
fn fill(ed: &mut EditorState) {
    ed.update(EditorMsg::TunnelName(0, "db".into()));
    ed.update(EditorMsg::TunnelPort(0, "5432".into()));
    ed.update(EditorMsg::TunnelHost(0, "db.internal".into()));
    ed.update(EditorMsg::TunnelHostPort(0, "5432".into()));
}

fn refused(ed: &mut EditorState, store: &mut ProfileStore) -> Vec<ValidationError> {
    match ed.save(store, &file()) {
        Err(SaveError::Invalid(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn blank_target_port_blocks_save() {
    let (mut ed, _, mut store) = with_tunnel();
    fill(&mut ed);
    ed.update(EditorMsg::TunnelHostPort(0, String::new()));
    let err = ValidationError::TunnelTargetPort {
        profile: "web".into(),
        name: "db".into(),
    };
    assert_eq!(refused(&mut ed, &mut store), vec![err.clone()]);
    assert_eq!(err.to_string(), "Tunnel \"db\" in web needs a target port");
    // The same on a remote tunnel; SOCKS has no target.
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Remote));
    assert_eq!(refused(&mut ed, &mut store), vec![err]);
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Socks));
    ed.save(&mut store, &file()).unwrap();
}

#[test]
fn empty_name_and_zero_ports_block_save() {
    let (mut ed, _, mut store) = with_tunnel();
    fill(&mut ed);
    ed.update(EditorMsg::TunnelName(0, " ".into()));
    assert_eq!(
        refused(&mut ed, &mut store),
        vec![ValidationError::EmptyTunnelName {
            profile: "web".into()
        }]
    );
    ed.update(EditorMsg::TunnelName(0, "db".into()));
    ed.update(EditorMsg::TunnelPort(0, String::new()));
    let port0 = ValidationError::TunnelPort {
        profile: "web".into(),
        name: "db".into(),
    };
    assert_eq!(refused(&mut ed, &mut store), vec![port0.clone()]);
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Socks));
    assert_eq!(refused(&mut ed, &mut store), vec![port0]);
    // A remote listen port may be blank: the server picks.
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Remote));
    ed.update(EditorMsg::TunnelHost(0, "localhost".into()));
    ed.update(EditorMsg::TunnelHostPort(0, "22".into()));
    ed.save(&mut store, &file()).unwrap();
}

#[test]
fn untouched_tunnels_round_trip() {
    let mut web = ssh("web", "10.0.0.5", vec![]);
    let defs = vec![
        TunnelDef {
            name: "db".into(),
            kind: TunnelKind::Remote {
                bind: "0.0.0.0".into(),
                port: 0,
                host: "localhost".into(),
                host_port: 22,
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
    if let ProfileKind::Ssh(s) = &mut web.kind {
        s.tunnels = defs.clone();
    }
    let mut store = ProfileStore {
        profiles: vec![web.clone()],
    };
    let (mut ed, _) = editor(&store);
    ed.update(EditorMsg::Name("web2".into()));
    ed.save(&mut store, &file()).unwrap();
    let ProfileKind::Ssh(s) = &store.profiles[0].kind else {
        unreachable!()
    };
    assert_eq!(s.tunnels, defs);
}

#[test]
fn local_target_survives_a_socks_detour() {
    let (mut ed, id, _) = with_tunnel();
    fill(&mut ed);
    let before = tunnels_of(&ed, id);
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Socks));
    ed.update(EditorMsg::TunnelKind(0, KindChoice::Local));
    assert_eq!(tunnels_of(&ed, id), before);
}

#[test]
fn port_filter_is_digits_only() {
    let (mut ed, id, _) = with_tunnel();
    ed.update(EditorMsg::TunnelPort(0, "+80".into()));
    assert!(matches!(
        tunnels_of(&ed, id)[0].kind,
        TunnelKind::Local { port: 0, .. }
    ));
}
