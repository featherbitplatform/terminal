//! Editor state rules.

use super::*;
use fbterm_secrets::{MemoryStore, SecretError};
use parking_lot::Mutex;

/// Records every call, backed by memory.
#[derive(Default)]
struct Recording {
    inner: MemoryStore,
    removed: Mutex<Vec<ProfileId>>,
}

impl SecretStore for Recording {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        self.inner.get(id, kind)
    }
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        self.inner.set(id, kind, value)
    }
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        self.removed.lock().push(*id);
        self.inner.remove(id)
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
    let errs = ed.apply(&mut store).unwrap_err();
    assert_eq!(errs, vec![ValidationError::EmptyHost(name)]);
    assert_eq!(ed.errors, errs);
    assert_eq!(store, before, "an invalid store is never applied");

    ed.update(EditorMsg::Host("10.0.0.5".into()));
    ed.apply(&mut store).unwrap();
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
    ed.apply(&mut store).unwrap();
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
    ed.apply(&mut store).unwrap();
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
    ed.update(EditorMsg::Secret("hunter2-secret".into()));
    ed.apply(&mut store).unwrap();
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
    ed.apply(&mut store).unwrap();
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
