//! User authentication for one hop, following the profile's [`AuthMethod`].
//!
//! Secrets come from the store first, then from the prompter. A secret is written to the
//! store only after the server (or, for a key passphrase, the key file) accepted it AND the
//! user asked to remember it; a saved secret that fails is left in place until a replacement
//! succeeds. Secret values are never logged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fbterm_config::{AuthMethod, Profile, SshProfile};
use fbterm_secrets::{SecretError, SecretKind, SecretStore, Zeroizing};
use russh::client::Handle;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg};

use super::{ClientHandler, SecretAnswer, SshContext, SshError};
use crate::AuthWarning;

/// Prompts per secret before giving up.
const MAX_PROMPTS: usize = 3;

/// What a successful authentication turned up.
pub(super) struct Authenticated {
    /// Something the user should hear about (an RSA user key).
    pub warning: Option<AuthWarning>,
    /// The key file the user chose for a key profile that names none.
    pub chosen_key: Option<PathBuf>,
}

impl From<Option<AuthWarning>> for Authenticated {
    fn from(warning: Option<AuthWarning>) -> Self {
        Self {
            warning,
            chosen_key: None,
        }
    }
}

pub(super) async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    ssh: &SshProfile,
    ctx: &SshContext,
) -> Result<Authenticated, SshError> {
    match &ssh.auth {
        AuthMethod::Password => password(handle, profile, ssh, ctx)
            .await
            .map(|()| None.into()),
        AuthMethod::Key {
            path: Some(path), ..
        } => key(handle, profile, ssh, ctx, path).await.map(Into::into),
        AuthMethod::Key { path: None, .. } => chosen_key(handle, profile, ssh, ctx).await,
        AuthMethod::Agent => agent(handle, profile, ssh).await.map(Into::into),
    }
}

/// A key profile that names no key file yet: ask the user for one, log in with it like a
/// configured key, and once the server accepted it tell the prompter, which remembers it in
/// the profile. A connect that cannot ask (automatic reconnects) needs the user at once.
async fn chosen_key(
    handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    ssh: &SshProfile,
    ctx: &SshContext,
) -> Result<Authenticated, SshError> {
    if !ctx.prompter.can_ask() {
        return Err(SshError::NeedsInput);
    }
    let path = ctx
        .prompter
        .ask_key_file(&profile.name)
        .await
        .ok_or(SshError::Cancelled)?;
    let warning = key(handle, profile, ssh, ctx, &path).await?;
    ctx.prompter.key_file_accepted(profile.id, &path);
    Ok(Authenticated {
        warning,
        chosen_key: Some(path),
    })
}

fn failed(profile: &Profile) -> SshError {
    SshError::AuthFailed {
        profile: profile.name.clone(),
    }
}

/// Run a secret-store call on the blocking pool: vault calls wait on a write or run scrypt,
/// which must not hold up the connection's task.
async fn off_task<T: Send + 'static>(
    ctx: &SshContext,
    call: impl FnOnce(&dyn SecretStore) -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    let secrets = ctx.secrets.clone();
    tokio::task::spawn_blocking(move || call(secrets.as_ref()))
        .await
        .unwrap_or_else(|e| {
            Err(SecretError::Storage(format!(
                "The saved secrets call did not finish ({e})."
            )))
        })
}

/// The saved secret, if any. A locked vault is first unlocked with the main password the
/// prompter gives; a cancel (or a prompter that cannot ask) means none is saved. A store
/// error is logged (without the value) and treated as none.
async fn saved(ctx: &SshContext, profile: &Profile, kind: SecretKind) -> Option<String> {
    let id = profile.id;
    let result = match off_task(ctx, move |s| s.get(&id, kind)).await {
        Err(SecretError::Locked) => {
            let password = ctx.prompter.unlock_vault().await?;
            match off_task(ctx, move |s| s.unlock_vault(&password)).await {
                Ok(()) => off_task(ctx, move |s| s.get(&id, kind)).await,
                Err(e) => Err(e),
            }
        }
        other => other,
    };
    match result {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, ?kind, "cannot read saved secret");
            None
        }
    }
}

/// Save an accepted secret if the user asked to.
async fn remember(ctx: &SshContext, profile: &Profile, kind: SecretKind, answer: &SecretAnswer) {
    if !answer.remember {
        return;
    }
    let id = profile.id;
    let value = Zeroizing::new(answer.value.clone());
    let save = || {
        let value = value.clone();
        off_task(ctx, move |s| s.set(&id, kind, &value))
    };
    let result = match save().await {
        // The vault is in use and has no file yet: ask for a main password, make the vault
        // and save again. A cancel leaves the secret unsaved.
        Err(SecretError::NoMainPassword) => match ctx.prompter.set_vault_password().await {
            // An empty answer: no longer needed (made meanwhile). Never make a vault with an
            // empty main password; just save once more, and if that fails it is not saved.
            Some(password) if password.is_empty() => save().await,
            Some(password) => match off_task(ctx, move |s| s.create_vault(&password)).await {
                // Made meanwhile (another connect asked too): it is open now.
                Ok(()) | Err(SecretError::VaultExists) => save().await,
                Err(e) => Err(e),
            },
            None => {
                tracing::info!(?kind, "secret not saved: no main password was set");
                return;
            }
        },
        other => other,
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, ?kind, "cannot save secret");
    }
}

async fn password(
    handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    ssh: &SshProfile,
    ctx: &SshContext,
) -> Result<(), SshError> {
    let mut retry = false;
    if let Some(pw) = saved(ctx, profile, SecretKind::Password).await {
        if handle.authenticate_password(&ssh.user, pw).await?.success() {
            return Ok(());
        }
        tracing::info!(profile = %profile.name, "saved password rejected");
        retry = true;
    }
    for _ in 0..MAX_PROMPTS {
        let answer = ctx
            .prompter
            .ask_secret(&profile.name, SecretKind::Password, retry)
            .await
            .ok_or(SshError::Cancelled)?;
        if handle
            .authenticate_password(&ssh.user, answer.value.clone())
            .await?
            .success()
        {
            remember(ctx, profile, SecretKind::Password, &answer).await;
            return Ok(());
        }
        retry = true;
    }
    Err(failed(profile))
}

/// `~/...` paths are relative to the home directory.
fn expand_home(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(dirs) = directories::BaseDirs::new()
    {
        return dirs.home_dir().join(rest);
    }
    path.to_path_buf()
}

/// Load the key file; `Ok(None)` means it needs a (different) passphrase.
fn load(path: &Path, passphrase: Option<&str>) -> Result<Option<PrivateKey>, SshError> {
    match russh::keys::load_secret_key(path, passphrase) {
        Ok(k) => Ok(Some(k)),
        Err(russh::keys::Error::KeyIsEncrypted) => Ok(None),
        // With a passphrase, a decoding failure almost always means a wrong passphrase.
        Err(_) if passphrase.is_some() => Ok(None),
        Err(e) => Err(SshError::Protocol(format!(
            "cannot load key {}: {e}",
            path.display()
        ))),
    }
}

/// Decrypt the key file: saved passphrase first, then up to [`MAX_PROMPTS`] prompts. Returns
/// the key and the typed answer to remember once the server accepts the key.
async fn unlock(
    path: &Path,
    profile: &Profile,
    ctx: &SshContext,
) -> Result<(PrivateKey, Option<SecretAnswer>), SshError> {
    if let Some(k) = load(path, None)? {
        return Ok((k, None));
    }
    let mut retry = false;
    if let Some(pass) = saved(ctx, profile, SecretKind::KeyPassphrase).await {
        if let Some(k) = load(path, Some(&pass))? {
            return Ok((k, None));
        }
        tracing::info!(profile = %profile.name, "saved key passphrase rejected");
        retry = true;
    }
    for _ in 0..MAX_PROMPTS {
        let answer = ctx
            .prompter
            .ask_secret(&profile.name, SecretKind::KeyPassphrase, retry)
            .await
            .ok_or(SshError::Cancelled)?;
        if let Some(k) = load(path, Some(&answer.value))? {
            return Ok((k, Some(answer)));
        }
        retry = true;
    }
    Err(failed(profile))
}

/// The RSA signature hash to use with this server (`None` for non-RSA keys).
async fn rsa_hash(
    handle: &Handle<ClientHandler>,
    algorithm: &russh::keys::Algorithm,
) -> Result<Option<HashAlg>, SshError> {
    if !matches!(algorithm, russh::keys::Algorithm::Rsa { .. }) {
        return Ok(None);
    }
    Ok(rsa_hash_for(handle.best_supported_rsa_hash().await?))
}

/// Pick the RSA signature hash from what the server announced in `server-sig-algs`
/// (`None`: it announced nothing; `Some(None)`: it supports only legacy SHA-1 `ssh-rsa`).
///
/// A server that announces nothing gets `rsa-sha2-512`, as OpenSSH does: SHA-1 signatures are
/// refused by default by current servers, and are only used when the server lists nothing else.
fn rsa_hash_for(server_sig_algs: Option<Option<HashAlg>>) -> Option<HashAlg> {
    server_sig_algs.unwrap_or(Some(HashAlg::Sha512))
}

async fn key(
    handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    ssh: &SshProfile,
    ctx: &SshContext,
    path: &Path,
) -> Result<Option<AuthWarning>, SshError> {
    let path = expand_home(path);
    let (key, answer) = unlock(&path, profile, ctx).await?;
    let warning = rsa_warning(&key.algorithm());
    let hash = rsa_hash(handle, &key.algorithm()).await?;
    let accepted = handle
        .authenticate_publickey(&ssh.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await?
        .success();
    if !accepted {
        return Err(failed(profile));
    }
    if let Some(answer) = answer {
        remember(ctx, profile, SecretKind::KeyPassphrase, &answer).await;
    }
    Ok(warning)
}

/// Signing with an RSA user key is worth a warning (timing side channel in the `rsa` crate);
/// verifying a server's RSA host key is not affected and never warns.
fn rsa_warning(algorithm: &russh::keys::Algorithm) -> Option<AuthWarning> {
    matches!(algorithm, russh::keys::Algorithm::Rsa { .. }).then_some(AuthWarning::RsaUserKey)
}

#[cfg(unix)]
async fn agent(
    handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    ssh: &SshProfile,
) -> Result<Option<AuthWarning>, SshError> {
    use russh::keys::agent::AgentIdentity;
    use russh::keys::agent::client::AgentClient;

    let mut client = match AgentClient::connect_env().await {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(error = %e, "no usable ssh-agent (SSH_AUTH_SOCK)");
            return Err(failed(profile));
        }
    };
    let identities = match client.request_identities().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::info!(error = %e, "ssh-agent did not list its keys");
            return Err(failed(profile));
        }
    };
    for identity in identities {
        // Certificates are not supported yet; plain keys only.
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let warning = rsa_warning(&key.algorithm());
        let hash = rsa_hash(handle, &key.algorithm()).await?;
        match handle
            .authenticate_publickey_with(&ssh.user, key, hash, &mut client)
            .await
        {
            Ok(result) if result.success() => return Ok(warning),
            Ok(_) => {}
            Err(e) => tracing::info!(error = %e, "ssh-agent could not sign"),
        }
    }
    Err(failed(profile))
}

/// Windows agents (OpenSSH named pipe, Pageant) are not wired up yet.
#[cfg(not(unix))]
async fn agent(
    _handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    _ssh: &SshProfile,
) -> Result<Option<AuthWarning>, SshError> {
    Err(SshError::Protocol(format!(
        "profile {:?}: ssh-agent authentication is not available on this platform yet",
        profile.name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rsa_user_keys_warn() {
        use russh::keys::Algorithm;
        assert_eq!(
            rsa_warning(&Algorithm::Rsa { hash: None }),
            Some(AuthWarning::RsaUserKey)
        );
        assert_eq!(rsa_warning(&Algorithm::Ed25519), None);
    }

    mod vault {
        use super::*;
        use crate::ssh::{NoPrompt, Prompter};
        use fbterm_config::{ProfileId, ProfileKind, SecretStorage};
        use fbterm_secrets::{MemoryStore, SecretBackend, SecretStore, Zeroizing};
        use std::sync::atomic::{AtomicUsize, Ordering};

        const MAIN: &str = "main-pw-auth-tests";

        /// Answers `Unlock saved secrets` with `answer` and counts the questions; answers
        /// `Set a main password` with `set` and counts those.
        struct Unlocker {
            answer: Option<&'static str>,
            asked: AtomicUsize,
            set: Option<&'static str>,
            set_asked: AtomicUsize,
            /// Makes this vault (main password `MAIN`) before answering `Set`, as Settings or
            /// another connect would meanwhile.
            made_meanwhile: Option<Arc<SecretBackend>>,
        }

        #[async_trait::async_trait]
        impl Prompter for Unlocker {
            async fn confirm_host_key(&self, _: &str, _: u16, _: &str) -> bool {
                false
            }
            async fn ask_secret(
                &self,
                _: &str,
                _: SecretKind,
                _: bool,
            ) -> Option<crate::ssh::SecretAnswer> {
                None
            }
            async fn ask_key_file(&self, _: &str) -> Option<PathBuf> {
                None
            }
            async fn unlock_vault(&self) -> Option<Zeroizing<String>> {
                self.asked.fetch_add(1, Ordering::SeqCst);
                self.answer.map(|a| Zeroizing::new(a.to_owned()))
            }
            async fn set_vault_password(&self) -> Option<Zeroizing<String>> {
                self.set_asked.fetch_add(1, Ordering::SeqCst);
                if let Some(b) = &self.made_meanwhile {
                    b.create_vault(MAIN).unwrap();
                }
                self.set.map(|a| Zeroizing::new(a.to_owned()))
            }
        }

        fn unlocker(answer: Option<&'static str>, set: Option<&'static str>) -> Arc<Unlocker> {
            Arc::new(Unlocker {
                answer,
                asked: AtomicUsize::new(0),
                set,
                set_asked: AtomicUsize::new(0),
                made_meanwhile: None,
            })
        }

        /// The vault selected, no vault file yet (after Auto moved or a reset).
        fn no_vault(dir: &Path) -> Arc<SecretBackend> {
            Arc::new(SecretBackend::with_system(
                SecretStorage::Vault,
                Box::new(MemoryStore::new()),
                dir.join("secrets.fbvault"),
            ))
        }

        fn typed(value: &str) -> crate::ssh::SecretAnswer {
            crate::ssh::SecretAnswer {
                value: value.into(),
                remember: true,
            }
        }

        #[tokio::test]
        async fn remember_with_no_vault_sets_a_main_password() {
            let dir = tempfile::tempdir().unwrap();
            let p = profile();
            let secrets = no_vault(dir.path());
            let asker = unlocker(None, Some(MAIN));
            let ctx = context(secrets.clone(), asker.clone());
            remember(&ctx, &p, SecretKind::Password, &typed("typed-pw")).await;
            assert_eq!(asker.set_asked.load(Ordering::SeqCst), 1);
            assert!(secrets.vault_exists() && secrets.vault_unlocked());
            assert_eq!(
                secrets.get(&p.id, SecretKind::Password).unwrap().as_deref(),
                Some("typed-pw")
            );
            // The vault now exists: the next remember just saves.
            remember(&ctx, &p, SecretKind::KeyPassphrase, &typed("pp")).await;
            assert_eq!(asker.set_asked.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn an_empty_set_answer_never_makes_a_vault() {
            // The app answers "" when Set is no longer needed; with no vault after all,
            // nothing is made (a vault anyone could open) and the secret is not saved.
            let dir = tempfile::tempdir().unwrap();
            let p = profile();
            let secrets = no_vault(dir.path());
            let asker = unlocker(None, Some(""));
            let ctx = context(secrets.clone(), asker.clone());
            remember(&ctx, &p, SecretKind::Password, &typed("typed-pw")).await;
            assert_eq!(asker.set_asked.load(Ordering::SeqCst), 1);
            assert!(!secrets.vault_path().exists());
            assert!(!secrets.vault_exists() && !secrets.vault_unlocked());
            assert_eq!(secrets.get(&p.id, SecretKind::Password).unwrap(), None);
        }

        #[tokio::test]
        async fn set_no_longer_needed_saves_into_the_vault_made_meanwhile() {
            let dir = tempfile::tempdir().unwrap();
            let p = profile();
            let secrets = no_vault(dir.path());
            let asker = Arc::new(Unlocker {
                answer: None,
                asked: AtomicUsize::new(0),
                set: Some(""),
                set_asked: AtomicUsize::new(0),
                made_meanwhile: Some(secrets.clone()),
            });
            let ctx = context(secrets.clone(), asker.clone());
            remember(&ctx, &p, SecretKind::Password, &typed("typed-pw")).await;
            assert_eq!(asker.set_asked.load(Ordering::SeqCst), 1);
            assert_eq!(
                secrets.get(&p.id, SecretKind::Password).unwrap().as_deref(),
                Some("typed-pw")
            );
            // The vault is the one made meanwhile, with its own main password.
            let fresh = fbterm_secrets::VaultStore::new();
            fresh.unlock(secrets.vault_path(), MAIN).unwrap();
            assert_eq!(
                fresh.get(&p.id, SecretKind::Password).unwrap().as_deref(),
                Some("typed-pw")
            );
        }

        #[tokio::test]
        async fn cancel_set_password_leaves_the_secret_unsaved() {
            let dir = tempfile::tempdir().unwrap();
            let p = profile();
            let secrets = no_vault(dir.path());
            let asker = unlocker(None, None);
            let ctx = context(secrets.clone(), asker.clone());
            remember(&ctx, &p, SecretKind::Password, &typed("typed-pw")).await;
            assert_eq!(asker.set_asked.load(Ordering::SeqCst), 1);
            assert!(!secrets.vault_exists());
            assert_eq!(secrets.get(&p.id, SecretKind::Password).unwrap(), None);
            // Nobody to ask: nothing asked, nothing saved.
            let ctx = SshContext {
                prompter: Arc::new(NoPrompt),
                ..ctx
            };
            remember(&ctx, &p, SecretKind::Password, &typed("typed-pw")).await;
            assert!(!secrets.vault_exists());
        }

        fn profile() -> Profile {
            Profile {
                id: ProfileId::new(),
                name: "db".into(),
                group: None,
                accent: None,
                accent_style: Default::default(),
                color_scheme: None,
                kind: ProfileKind::Ssh(SshProfile {
                    host: "db.example.com".into(),
                    port: 22,
                    user: "ops".into(),
                    auth: AuthMethod::Password,
                    jump: vec![],
                    tunnels: Vec::new(),
                }),
            }
        }

        /// A locked vault (in `dir`) holding `p`'s password.
        fn locked(dir: &Path, p: &Profile) -> Arc<SecretBackend> {
            let path = dir.join("secrets.fbvault");
            let setup = SecretBackend::with_system(
                SecretStorage::Vault,
                Box::new(MemoryStore::new()),
                path.clone(),
            );
            setup.create_vault(MAIN).unwrap();
            setup.set(&p.id, SecretKind::Password, "saved-pw").unwrap();
            Arc::new(SecretBackend::with_system(
                SecretStorage::Vault,
                Box::new(MemoryStore::new()),
                path,
            ))
        }

        fn context(secrets: Arc<SecretBackend>, prompter: Arc<dyn Prompter>) -> SshContext {
            SshContext {
                known_hosts: PathBuf::from("known_hosts"),
                secrets,
                prompter,
            }
        }

        #[tokio::test]
        async fn cancel_unlock_treats_the_secret_as_not_saved() {
            let dir = tempfile::tempdir().unwrap();
            let p = profile();
            let secrets = locked(dir.path(), &p);
            let asker = unlocker(None, None);
            let ctx = context(secrets.clone(), asker.clone());
            assert_eq!(saved(&ctx, &p, SecretKind::Password).await, None);
            assert_eq!(asker.asked.load(Ordering::SeqCst), 1);
            assert!(!secrets.vault_unlocked());
            // Nobody to ask (automatic reconnects): not saved, and nothing is asked.
            let ctx = SshContext {
                prompter: Arc::new(NoPrompt),
                ..ctx
            };
            assert_eq!(saved(&ctx, &p, SecretKind::Password).await, None);
        }

        #[tokio::test]
        async fn unlock_answer_opens_the_vault_for_the_saved_secret() {
            let dir = tempfile::tempdir().unwrap();
            let p = profile();
            let secrets = locked(dir.path(), &p);
            let asker = unlocker(Some(MAIN), None);
            let ctx = context(secrets.clone(), asker.clone());
            assert_eq!(
                saved(&ctx, &p, SecretKind::Password).await.as_deref(),
                Some("saved-pw")
            );
            assert!(secrets.vault_unlocked());
            // Open now: no second question.
            assert_eq!(
                saved(&ctx, &p, SecretKind::Password).await.as_deref(),
                Some("saved-pw")
            );
            assert_eq!(asker.asked.load(Ordering::SeqCst), 1);

            // A wrong main password is not a saved secret either.
            let other = profile();
            let other_dir = tempfile::tempdir().unwrap();
            let secrets = locked(other_dir.path(), &other);
            let wrong = unlocker(Some("wrong"), None);
            let ctx = context(secrets, wrong);
            assert_eq!(saved(&ctx, &other, SecretKind::Password).await, None);
        }

        #[tokio::test]
        async fn remember_saves_off_the_calling_task() {
            let p = profile();
            let store = Arc::new(MemoryStore::new());
            let ctx = SshContext {
                known_hosts: PathBuf::from("known_hosts"),
                secrets: store.clone(),
                prompter: Arc::new(NoPrompt),
            };
            let answer = crate::ssh::SecretAnswer {
                value: "typed".into(),
                remember: true,
            };
            remember(&ctx, &p, SecretKind::Password, &answer).await;
            assert_eq!(
                store.get(&p.id, SecretKind::Password).unwrap().as_deref(),
                Some("typed")
            );
        }
    }

    #[test]
    fn rsa_hash_selection() {
        // Nothing announced: rsa-sha2-512, never legacy ssh-rsa.
        assert_eq!(rsa_hash_for(None), Some(HashAlg::Sha512));
        // Announced: the server's strongest choice, legacy only when that is all it has.
        assert_eq!(
            rsa_hash_for(Some(Some(HashAlg::Sha256))),
            Some(HashAlg::Sha256)
        );
        assert_eq!(
            rsa_hash_for(Some(Some(HashAlg::Sha512))),
            Some(HashAlg::Sha512)
        );
        assert_eq!(rsa_hash_for(Some(None)), None);
    }
}
