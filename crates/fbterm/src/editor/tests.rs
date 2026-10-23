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
        kind: ProfileKind::Ssh(SshProfile {
            host: host.into(),
            port: 22,
            user: "fb".into(),
            auth: AuthMethod::Agent,
            jump,
        }),
    }
}

fn local(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
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
