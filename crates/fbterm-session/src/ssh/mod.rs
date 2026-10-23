//! SSH connections: transport, host-key trust and authentication for one hop.
//!
//! [`connect_hop`] runs SSH over any byte stream (a TCP socket for the first hop, a
//! `direct-tcpip` channel for later ones), checks the server's host key against
//! `known_hosts` and authenticates with the profile's [`AuthMethod`]. Questions for the user
//! (trust this host? which password?) go through a [`Prompter`]; saved secrets come from a
//! [`SecretStore`].
//!
//! Security rules enforced here:
//! - a changed host key is refused without asking, and `known_hosts` is left untouched;
//! - an unknown host key is trusted only when the user confirms its SHA-256 fingerprint;
//! - a secret is saved only after the server accepted it and the user asked to remember it,
//!   and a saved secret is never removed because it was rejected;
//! - secret values are never logged and never appear in an [`SshError`].

mod auth;
mod known_hosts;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fbterm_config::{Profile, ProfileKind, ValidationError};
use fbterm_secrets::{SecretKind, SecretStore};
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PublicKey, PublicKeyOrCertificate};
use tokio::io::{AsyncRead, AsyncWrite};

pub use known_hosts::{HostKeyStatus, check_host_key, learn_host_key};

/// Seconds of silence from the server before a keepalive is sent.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Unanswered keepalives after which the connection is considered dead.
const KEEPALIVE_MAX: usize = 3;

/// Asks the user the questions a connection needs answered.
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Trust-on-first-use: should this unknown host key be trusted (and remembered)?
    /// `fingerprint_sha256` is OpenSSH style, `SHA256:<base64 without padding>`.
    async fn confirm_host_key(&self, host: &str, port: u16, fingerprint_sha256: &str) -> bool;

    /// Ask for a password or key passphrase. `retry` is true when a previous value (saved or
    /// typed) was rejected. `None` means the user cancelled.
    async fn ask_secret(
        &self,
        profile_name: &str,
        kind: SecretKind,
        retry: bool,
    ) -> Option<SecretAnswer>;
}

/// A secret typed by the user, and whether to save it once the server accepts it.
#[derive(Clone)]
pub struct SecretAnswer {
    pub value: String,
    pub remember: bool,
}

impl std::fmt::Debug for SecretAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretAnswer")
            .field("value", &"<redacted>")
            .field("remember", &self.remember)
            .finish()
    }
}

/// Everything a connection needs besides the profile itself.
#[derive(Clone)]
pub struct SshContext {
    /// The `known_hosts` file to check and extend (normally `~/.ssh/known_hosts`).
    pub known_hosts: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
    pub prompter: Arc<dyn Prompter>,
}

/// Why a connection failed. Messages never contain secret values.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error(
        "the host key of {host} port {port} has changed; refusing to connect. Someone may be \
         intercepting the connection. If the server was reinstalled, remove its old entry from \
         known_hosts and connect again."
    )]
    HostKeyChanged { host: String, port: u16 },
    #[error("the host key was not trusted")]
    HostKeyRejected,
    #[error("authentication failed for {profile:?}")]
    AuthFailed { profile: String },
    #[error("cancelled")]
    Cancelled,
    #[error("could not connect: {0}")]
    Connect(std::io::Error),
    #[error("invalid jump chain: {0}")]
    Jump(ValidationError),
    #[error("ssh error: {0}")]
    Protocol(String),
}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        match e {
            russh::Error::IO(io) => Self::Connect(io),
            other => Self::Protocol(other.to_string()),
        }
    }
}

/// The russh client handler of one hop: verifies the server's host key.
pub struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    prompter: Arc<dyn Prompter>,
    /// The key accepted for this connection, so a re-key does not ask again.
    accepted: Option<PublicKey>,
}

impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, SshError> {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                return Err(SshError::Protocol(
                    "host certificates are not supported".into(),
                ));
            }
        };
        if self
            .accepted
            .as_ref()
            .is_some_and(|a| a.key_data() == key.key_data())
        {
            return Ok(true);
        }
        match check_host_key(&self.known_hosts, &self.host, self.port, key)? {
            HostKeyStatus::Known => {}
            HostKeyStatus::Changed => {
                tracing::warn!(host = %self.host, port = self.port, "host key changed; refusing");
                return Err(SshError::HostKeyChanged {
                    host: self.host.clone(),
                    port: self.port,
                });
            }
            HostKeyStatus::Unknown => {
                let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
                if !self
                    .prompter
                    .confirm_host_key(&self.host, self.port, &fingerprint)
                    .await
                {
                    return Err(SshError::HostKeyRejected);
                }
                // Like OpenSSH: failing to record the key is reported but does not undo the
                // user's decision for this connection.
                if let Err(e) = learn_host_key(&self.known_hosts, &self.host, self.port, key) {
                    tracing::warn!(error = %e, "could not record host key in known_hosts");
                }
            }
        }
        self.accepted = Some(key.clone());
        Ok(true)
    }
}

/// Client configuration for one hop: keepalives on, known host-key types preferred.
fn client_config(host: &str, port: u16, known_hosts: &std::path::Path) -> client::Config {
    let mut preferred = russh::Preferred::default();
    let known = known_hosts::known_algorithms(known_hosts, host, port);
    if !known.is_empty() {
        // Ask for a key type we already trust first, so a server with several host keys does
        // not look "changed" just because it offered a different type.
        let (mut order, rest): (Vec<_>, Vec<_>) = preferred
            .key
            .iter()
            .cloned()
            .partition(|a| known.contains(a));
        order.extend(rest);
        preferred.key = order.into();
    }
    client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: KEEPALIVE_MAX,
        nodelay: true,
        preferred,
        ..Default::default()
    }
}

/// Run SSH over `stream` to the host of `profile`: verify its host key and authenticate.
pub(crate) async fn connect_hop<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    profile: &Profile,
    ctx: &SshContext,
) -> Result<Handle<ClientHandler>, SshError> {
    let ProfileKind::Ssh(ssh) = &profile.kind else {
        return Err(SshError::Protocol(format!(
            "profile {:?} is not an SSH profile",
            profile.name
        )));
    };
    let handler = ClientHandler {
        host: ssh.host.clone(),
        port: ssh.port,
        known_hosts: ctx.known_hosts.clone(),
        prompter: ctx.prompter.clone(),
        accepted: None,
    };
    let config = client_config(&ssh.host, ssh.port, &ctx.known_hosts);
    let mut handle = client::connect_stream(Arc::new(config), stream, handler).await?;
    auth::authenticate(&mut handle, profile, ssh, ctx).await?;
    Ok(handle)
}

/// Connect straight to the profile's host over TCP (no jump chain).
///
/// Profiles with a jump chain are refused rather than silently connected directly.
pub async fn connect_direct(
    profile: &Profile,
    ctx: &SshContext,
) -> Result<Handle<ClientHandler>, SshError> {
    let ProfileKind::Ssh(ssh) = &profile.kind else {
        return Err(SshError::Protocol(format!(
            "profile {:?} is not an SSH profile",
            profile.name
        )));
    };
    if !ssh.jump.is_empty() {
        return Err(SshError::Protocol(format!(
            "profile {:?} has a jump chain; it cannot be connected directly",
            profile.name
        )));
    }
    let stream = tokio::net::TcpStream::connect((ssh.host.as_str(), ssh.port))
        .await
        .map_err(SshError::Connect)?;
    if let Err(e) = stream.set_nodelay(true) {
        tracing::debug!(error = %e, "set_nodelay failed");
    }
    connect_hop(stream, profile, ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_answer_debug_redacts_value() {
        let a = SecretAnswer {
            value: "hunter2".into(),
            remember: true,
        };
        let s = format!("{a:?}");
        assert!(!s.contains("hunter2"), "{s}");
        assert!(s.contains("redacted"));
    }

    #[test]
    fn known_key_types_are_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(
            &path,
            format!("[h]:2200 {}\n", known_hosts::tests_support::ECDSA),
        )
        .unwrap();
        let cfg = client_config("h", 2200, &path);
        assert_eq!(
            cfg.preferred.key[0],
            russh::keys::Algorithm::Ecdsa {
                curve: russh::keys::EcdsaCurve::NistP256
            }
        );
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(30)));
        assert_eq!(cfg.keepalive_max, 3);
        // Other hosts keep the default order.
        let other = client_config("other", 22, &path);
        assert_eq!(other.preferred.key[0], russh::keys::Algorithm::Ed25519);
    }
}
