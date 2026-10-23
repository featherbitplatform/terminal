//! User authentication for one hop, following the profile's [`AuthMethod`].
//!
//! Secrets come from the store first, then from the prompter. A secret is written to the
//! store only after the server (or, for a key passphrase, the key file) accepted it AND the
//! user asked to remember it; a saved secret that fails is left in place until a replacement
//! succeeds. Secret values are never logged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fbterm_config::{AuthMethod, Profile, SshProfile};
use fbterm_secrets::SecretKind;
use russh::client::Handle;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg};

use super::{ClientHandler, SecretAnswer, SshContext, SshError};
use crate::AuthWarning;

/// Prompts per secret before giving up.
const MAX_PROMPTS: usize = 3;

pub(super) async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    profile: &Profile,
    ssh: &SshProfile,
    ctx: &SshContext,
) -> Result<Option<AuthWarning>, SshError> {
    match &ssh.auth {
        AuthMethod::Password => password(handle, profile, ssh, ctx).await.map(|()| None),
        AuthMethod::Key { path, .. } => key(handle, profile, ssh, ctx, path).await,
        AuthMethod::Agent => agent(handle, profile, ssh).await,
    }
}

fn failed(profile: &Profile) -> SshError {
    SshError::AuthFailed {
        profile: profile.name.clone(),
    }
}

/// The saved secret, if any. A store error is logged (without the value) and treated as none.
fn saved(ctx: &SshContext, profile: &Profile, kind: SecretKind) -> Option<String> {
    match ctx.secrets.get(&profile.id, kind) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, ?kind, "cannot read saved secret");
            None
        }
    }
}

/// Save an accepted secret if the user asked to.
fn remember(ctx: &SshContext, profile: &Profile, kind: SecretKind, answer: &SecretAnswer) {
    if !answer.remember {
        return;
    }
    if let Err(e) = ctx.secrets.set(&profile.id, kind, &answer.value) {
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
    if let Some(pw) = saved(ctx, profile, SecretKind::Password) {
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
            remember(ctx, profile, SecretKind::Password, &answer);
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
    if let Some(pass) = saved(ctx, profile, SecretKind::KeyPassphrase) {
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
        remember(ctx, profile, SecretKind::KeyPassphrase, &answer);
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
