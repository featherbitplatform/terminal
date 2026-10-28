//! The saved-secret write of an editor Save or Duplicate outliving the editor window, against
//! a temporary config folder and in-memory secret storage (never the real keychain). The write
//! stays in flight (background work is not run here) until a test delivers its result.

use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, SshProfile};
use fbterm_secrets::SecretKind;

use super::duplicate::SAVING as DUPLICATE_SAVING;
use super::{DraftKind, EditorMsg, EditorState, JobKind, SAVING, SecretOp};
use crate::app::{App, Delivered, Message};
use crate::redact::Redacted;
use crate::restore_tests::paths_with;

const PW: &str = "job-pw-3b81";

fn ssh(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: format!("{name}.example.com"),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::Password,
            jump: Vec::new(),
            tunnels: Vec::new(),
        }),
    }
}

/// An app whose background work stays in flight.
fn app_with(profiles: &[Profile]) -> App {
    let (app, _) = App::build(paths_with(profiles), true);
    app
}

fn editor(app: &mut App, m: EditorMsg) {
    let _ = app.update(Message::Editor(m));
}

fn ed(app: &App) -> &EditorState {
    &app.editor.as_ref().expect("the editor is open").1
}

fn typed_secret(ed: &EditorState, id: ProfileId) -> String {
    match &ed.draft(id).unwrap().kind {
        DraftKind::Ssh(s) => s.secret.clone(),
        DraftKind::Local(_) => panic!("ssh"),
    }
}

/// Type `pw` as `id`'s password in the open editor and save: the secret write starts.
fn save_secret(app: &mut App, id: ProfileId, pw: &str) -> u64 {
    editor(app, EditorMsg::Select(id));
    editor(app, EditorMsg::Secret(Redacted(pw.into())));
    editor(app, EditorMsg::Save);
    let job = app.editor_job.expect("the secret write runs");
    assert_eq!(job.kind, JobKind::Save);
    job.seq
}

fn close_editor(app: &mut App) {
    let wid = app.editor_window().expect("the editor is open");
    let _ = app.update(Message::WindowClosed(wid));
    assert!(app.editor.is_none());
}

fn on_disk(app: &App) -> String {
    std::fs::read_to_string(app.paths.profiles()).unwrap()
}

fn saved(app: &mut App, seq: u64, results: Result<super::SecretResults, String>) {
    let _ = app.update(Message::EditorSecrets(seq, Delivered::new(results)));
}

#[test]
fn an_editor_reopened_mid_save_waits_for_it() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    assert_eq!(ed(&app).notice.as_deref(), Some(SAVING));
    close_editor(&mut app);
    assert!(
        app.editor_job.is_some(),
        "closing the editor keeps the hold"
    );

    editor(&mut app, EditorMsg::Open);
    assert_eq!(ed(&app).notice.as_deref(), Some(SAVING));
    // Save (its button is disabled meanwhile) and Duplicate are refused.
    let before = on_disk(&app);
    editor(&mut app, EditorMsg::Select(db.id));
    editor(&mut app, EditorMsg::Name("db-renamed".into()));
    editor(&mut app, EditorMsg::Save);
    assert_eq!(on_disk(&app), before);
    assert!(ed(&app).dirty);
    assert_eq!(app.editor_job.map(|j| j.seq), Some(seq));
    editor(&mut app, EditorMsg::Reload);
    editor(&mut app, EditorMsg::Duplicate);
    assert_eq!(ed(&app).notice.as_deref(), Some(DUPLICATE_SAVING));
    assert_eq!(on_disk(&app), before);

    // It ends well: nothing to say, and Save works again.
    saved(&mut app, seq, Ok(Vec::new()));
    assert!(app.editor_job.is_none());
    assert!(app.visible_notices().is_empty());
    editor(&mut app, EditorMsg::Name("db-renamed".into()));
    editor(&mut app, EditorMsg::Save);
    assert_ne!(on_disk(&app), before);
}

#[test]
fn a_reopened_editor_drops_the_saving_notice_when_the_write_succeeds() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    close_editor(&mut app);
    editor(&mut app, EditorMsg::Open);
    saved(&mut app, seq, Ok(Vec::new()));
    assert_eq!(ed(&app).notice, None);
    assert!(app.visible_notices().is_empty());
}

#[test]
fn a_failure_after_the_editor_closed_shows_on_the_banner() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    close_editor(&mut app);
    saved(&mut app, seq, Err("the vault is locked".into()));
    assert!(app.editor_job.is_none());
    assert!(app.visible_notices().contains(
        &"Profiles saved, but the saved secrets could not be changed: the vault is locked".into()
    ));

    // A change that failed on its own is named as the editor names it.
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    close_editor(&mut app);
    let op = SecretOp::Set {
        id: db.id,
        kind: SecretKind::Password,
        value: Redacted(PW.into()),
    };
    saved(&mut app, seq, Ok(vec![(op, Err("denied".into()))]));
    let said = "Profiles saved, but could not store the password of db: denied";
    assert!(app.visible_notices().contains(&said.into()));
    assert!(
        !app.notices.iter().any(|n| n.contains(PW)),
        "never the value"
    );
}

#[test]
fn a_result_after_close_and_reopen_leaves_the_new_editor_drafts() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    close_editor(&mut app);
    editor(&mut app, EditorMsg::Open);
    // The same password typed again in the new editor: the old save's success must not
    // take it for its own.
    editor(&mut app, EditorMsg::Select(db.id));
    editor(&mut app, EditorMsg::Secret(Redacted(PW.into())));
    let op = SecretOp::Set {
        id: db.id,
        kind: SecretKind::Password,
        value: Redacted(PW.into()),
    };
    saved(&mut app, seq, Ok(vec![(op.clone(), Ok(()))]));
    assert_eq!(typed_secret(ed(&app), db.id), PW);
    assert!(ed(&app).dirty);

    // A failure goes to the new editor's notice, its drafts untouched.
    close_editor(&mut app);
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    close_editor(&mut app);
    editor(&mut app, EditorMsg::Open);
    saved(&mut app, seq, Ok(vec![(op, Err("denied".into()))]));
    assert_eq!(
        ed(&app).notice.as_deref(),
        Some("Profiles saved, but could not store the password of db: denied")
    );
    assert!(!ed(&app).dirty);
    assert!(app.visible_notices().is_empty(), "said once, in the editor");
}

#[test]
fn a_duplicate_ending_after_the_editor_closed_is_reported_the_same_way() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    app.secrets.set(&db.id, SecretKind::Password, PW).unwrap();
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(db.id));
    editor(&mut app, EditorMsg::Duplicate);
    let job = app.editor_job.expect("the copy of its secrets runs");
    assert_eq!(job.kind, JobKind::Duplicate);
    close_editor(&mut app);
    // A save's result never ends a duplicate.
    saved(&mut app, job.seq, Ok(Vec::new()));
    assert_eq!(app.editor_job, Some(job));
    let _ = app.update(Message::EditorDuplicated(job.seq, Err("denied".into())));
    assert!(app.editor_job.is_none());
    assert!(
        app.visible_notices()
            .contains(&"Duplicated, but its saved secrets could not be copied: denied".into())
    );
}

#[test]
fn quit_waits_for_the_secret_write() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    app.state_saving_enabled = false;
    assert!(app.exit_now(), "nothing runs: quit exits at once");
    editor(&mut app, EditorMsg::Open);
    let seq = save_secret(&mut app, db.id, PW);
    assert!(!app.exit_now());
    let _ = app.quit();
    assert!(app.exit_waiting, "waits for the write");
    close_editor(&mut app);
    assert!(app.exit_waiting && !app.exit_now());
    saved(&mut app, seq, Ok(Vec::new()));
    assert!(app.exit_now(), "the write ended: the exit follows");
}
