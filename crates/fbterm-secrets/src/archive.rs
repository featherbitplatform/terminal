//! Profile archives (`.fbprofiles`): a versioned TOML document encrypted with `age` and a
//! passphrase (scrypt) recipient.
//!
//! The plaintext only ever exists in wiped buffers (`Zeroizing`); secret values and key file
//! bytes are wiped when an [`Archive`] is dropped and never appear in errors or `Debug`
//! output. Building an archive from the profile store and planning an import live elsewhere;
//! this module only seals and opens one.

use std::fmt;
use std::io::{Read, Write};
use std::iter;

use age::DecryptError;
use age::secrecy::SecretString;

use fbterm_config::schemes::Scheme;
use fbterm_config::{Profile, ProfileId};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::SecretKind;

/// The archive format this build writes and the newest it reads.
pub const FORMAT: u32 = 1;

/// The largest file `open` decrypts. Real archives are far smaller (key files are capped at
/// 1 MB each); the cap keeps a huge file from exhausting memory.
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// What an archive carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    /// Profiles with saved secrets and, optionally, key files: for the owner's own machines.
    Full,
    /// SSH profiles only, without secrets or key paths: for teammates.
    Shared,
}

/// The decrypted contents of a profile archive.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Archive {
    /// Format version; [`FORMAT`] for archives this build writes.
    pub format: u32,
    pub kind: ArchiveKind,
    /// When the archive was made (RFC 3339, informational).
    pub created: String,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub secrets: Vec<ArchivedSecret>,
    #[serde(default)]
    pub keys: Vec<ArchivedKey>,
    #[serde(default)]
    pub schemes: Vec<Scheme>,
}

impl Archive {
    /// An empty archive of the current format.
    pub fn new(kind: ArchiveKind, created: impl Into<String>) -> Self {
        Self {
            format: FORMAT,
            kind,
            created: created.into(),
            profiles: Vec::new(),
            secrets: Vec::new(),
            keys: Vec::new(),
            schemes: Vec::new(),
        }
    }
}

/// A saved password or key passphrase of one archived profile.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedSecret {
    pub profile: ProfileId,
    pub kind: SecretKind,
    pub value: Zeroizing<String>,
}

impl fmt::Debug for ArchivedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchivedSecret")
            .field("profile", &self.profile)
            .field("kind", &self.kind)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A private key file, stored once however many profiles use it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedKey {
    /// The file's name (no directories), used when writing it on import.
    pub file_name: String,
    /// The file's contents, base64 in the archive.
    #[serde(with = "base64_bytes")]
    pub bytes: Zeroizing<Vec<u8>>,
    /// The archived profiles that use this key.
    #[serde(default)]
    pub profiles: Vec<ProfileId>,
}

impl fmt::Debug for ArchivedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchivedKey")
            .field("file_name", &self.file_name)
            .field(
                "bytes",
                &format_args!("<{} bytes redacted>", self.bytes.len()),
            )
            .field("profiles", &self.profiles)
            .finish()
    }
}

/// Why an archive could not be sealed or opened. Every message is a plain sentence for the
/// user: no secret material and no raw crypto or parser output.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("Wrong password, or the file is not an fbterm archive.")]
    WrongPassword,
    #[error("This archive was made by a newer fbterm.")]
    Newer,
    #[error("{0}")]
    Corrupt(String),
    #[error("{0}")]
    Io(String),
}

const TOO_LARGE: &str = "The file is too large to be an fbterm archive.";
const DAMAGED: &str = "The archive is damaged or incomplete.";
const UNREADABLE: &str = "The archive's contents could not be read.";
const TOO_HARD: &str = "This archive takes too much work to open on this computer.";
const NOT_WRITTEN: &str = "The profiles could not be written to an archive.";

/// Encrypt `archive` with `password`: its TOML form inside an `age` file with a passphrase
/// (scrypt) recipient at age's default work factor (about a second on this machine).
///
/// Expects `archive.format == FORMAT`, which [`Archive::new`] sets: build archives through
/// it. The format is written as given (tests seal a newer one to check that `open` refuses
/// it), so nothing here checks it.
pub fn seal(archive: &Archive, password: &str) -> Result<Vec<u8>, ArchiveError> {
    let text = Zeroizing::new(
        toml::to_string(archive).map_err(|_| ArchiveError::Corrupt(NOT_WRITTEN.into()))?,
    );
    let encryptor = age::Encryptor::with_user_passphrase(SecretString::from(password));
    let failed = |_| ArchiveError::Io(NOT_WRITTEN.into());
    // Room for the header and the per-chunk tags, so the output rarely reallocates.
    let mut out = Vec::with_capacity(text.len() + text.len() / 1024 + 512);
    let mut writer = encryptor.wrap_output(&mut out).map_err(failed)?;
    writer.write_all(text.as_bytes()).map_err(failed)?;
    writer.finish().map_err(failed)?;
    Ok(out)
}

/// Decrypt and parse an archive. Anything that fails before the payload decrypts (not an
/// `age` file, another kind of `age` file, a wrong password, a damaged header) is
/// [`ArchiveError::WrongPassword`]; a damaged or cut-off payload, or contents that do not
/// parse, are [`ArchiveError::Corrupt`].
pub fn open(bytes: &[u8], password: &str) -> Result<Archive, ArchiveError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::Corrupt(TOO_LARGE.into()));
    }
    let decryptor = age::Decryptor::new_buffered(bytes).map_err(|_| ArchiveError::WrongPassword)?;
    // age caps the accepted work factor (about 16 times its own target), so a crafted file
    // cannot make this run for hours.
    let identity = age::scrypt::Identity::new(SecretString::from(password));
    let mut reader = decryptor
        .decrypt(iter::once(&identity as &dyn age::Identity))
        .map_err(|e| match e {
            DecryptError::ExcessiveWork { .. } => ArchiveError::Corrupt(TOO_HARD.into()),
            _ => ArchiveError::WrongPassword,
        })?;
    // The plaintext is shorter than the file, so this buffer never reallocates (which would
    // leave an unwiped copy behind).
    let mut plain = Zeroizing::new(Vec::with_capacity(bytes.len()));
    reader
        .read_to_end(&mut plain)
        .map_err(|_| ArchiveError::Corrupt(DAMAGED.into()))?;
    let text = std::str::from_utf8(&plain).map_err(|_| ArchiveError::Corrupt(UNREADABLE.into()))?;

    // The version first, so a newer layout is reported as such rather than as unreadable.
    #[derive(Deserialize)]
    struct Version {
        format: Option<u32>,
    }
    let version: Version =
        toml::from_str(text).map_err(|_| ArchiveError::Corrupt(UNREADABLE.into()))?;
    match version.format {
        Some(f) if f > FORMAT => return Err(ArchiveError::Newer),
        Some(f) if f >= 1 => {}
        _ => return Err(ArchiveError::Corrupt(UNREADABLE.into())),
    }
    // Parser messages quote the offending line, which may hold a secret: never pass them on.
    toml::from_str(text).map_err(|_| ArchiveError::Corrupt(UNREADABLE.into()))
}

/// Key file bytes as base64 text, through wiped buffers.
mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub fn serialize<S: Serializer>(bytes: &Zeroizing<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        let text = Zeroizing::new(STANDARD.encode(bytes.as_slice()));
        s.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Zeroizing<Vec<u8>>, D::Error> {
        let text = Zeroizing::<String>::deserialize(d)?;
        // Sized up front so decoding never reallocates and leaves an unwiped copy behind.
        let mut out = Zeroizing::new(Vec::with_capacity(text.len() / 4 * 3 + 3));
        STANDARD
            .decode_vec(text.as_bytes(), &mut out)
            .map_err(|_| serde::de::Error::custom("a key file is not valid base64"))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbterm_config::profile::{
        AuthMethod, LocalProfile, ProfileKind, SshProfile, TunnelDef, TunnelKind,
    };
    use fbterm_config::schemes::builtins;

    const PASSWORD: &str = "correct horse battery staple";
    const SECRET: &str = "hunter2-very-secret";
    const KEY_BYTES: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAsecretkeymaterial\n";

    /// Each seal or open costs about a second of scrypt (age's default work factor, which
    /// these tests keep), so the sample is sealed once and shared.
    fn sealed_sample() -> &'static (Archive, Vec<u8>) {
        static SEALED: std::sync::OnceLock<(Archive, Vec<u8>)> = std::sync::OnceLock::new();
        SEALED.get_or_init(|| {
            let archive = sample();
            let sealed = seal(&archive, PASSWORD).unwrap();
            (archive, sealed)
        })
    }

    fn sample() -> Archive {
        let bastion = Profile {
            id: ProfileId::new(),
            name: "bastion".into(),
            group: Some("prod".into()),
            accent: None,
            accent_style: Default::default(),
            color_scheme: Some("user-1".into()),
            kind: ProfileKind::Ssh(SshProfile {
                host: "bastion.example.com".into(),
                port: 22,
                user: "ops".into(),
                auth: AuthMethod::Key {
                    path: Some("/home/ops/.ssh/id_ed25519".into()),
                    passphrase: true,
                },
                jump: Vec::new(),
                tunnels: Vec::new(),
            }),
        };
        let db = Profile {
            id: ProfileId::new(),
            name: "db".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "10.0.0.5".into(),
                port: 2222,
                user: "postgres".into(),
                auth: AuthMethod::Password,
                jump: vec![bastion.id],
                tunnels: vec![TunnelDef {
                    name: "pg".into(),
                    kind: TunnelKind::Local {
                        bind: "127.0.0.1".into(),
                        port: 5432,
                        host: "localhost".into(),
                        host_port: 5432,
                    },
                    start: Default::default(),
                }],
            }),
        };
        let mut scheme = builtins().remove(1);
        scheme.id = "user-1".into();
        scheme.name = "Mine".into();
        scheme.builtin = false;
        scheme.font_size = Some(14.0);

        let mut archive = Archive::new(ArchiveKind::Full, "2026-10-27T10:00:00Z");
        archive.secrets = vec![
            ArchivedSecret {
                profile: db.id,
                kind: SecretKind::Password,
                value: Zeroizing::new(SECRET.into()),
            },
            ArchivedSecret {
                profile: bastion.id,
                kind: SecretKind::KeyPassphrase,
                value: Zeroizing::new("key-pass".into()),
            },
        ];
        archive.keys = vec![ArchivedKey {
            file_name: "id_ed25519".into(),
            bytes: Zeroizing::new(KEY_BYTES.to_vec()),
            profiles: vec![bastion.id],
        }];
        let shell = Profile {
            id: ProfileId::new(),
            name: "zsh".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Local(LocalProfile {
                program: "/bin/zsh".into(),
                args: vec!["-l".into()],
                cwd: None,
                env: [("TERM_THEME".to_string(), "dark".to_string())].into(),
            }),
        };
        archive.profiles = vec![bastion, db, shell];
        archive.schemes = vec![scheme];
        archive
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn seal_open_round_trip() {
        let (archive, sealed) = sealed_sample();
        assert!(sealed.starts_with(b"age-encryption.org/v1\n"));
        assert!(!contains(sealed, SECRET.as_bytes()));
        assert!(!contains(sealed, b"bastion"));
        let opened = open(sealed, PASSWORD).unwrap();
        assert_eq!(&opened, archive);
        assert_eq!(opened.keys[0].bytes.as_slice(), KEY_BYTES);
    }

    #[test]
    fn wrong_password_is_refused() {
        let (_, sealed) = sealed_sample();
        let err = open(sealed, "not the password").unwrap_err();
        assert_eq!(err, ArchiveError::WrongPassword);
        assert_eq!(
            err.to_string(),
            "Wrong password, or the file is not an fbterm archive."
        );
        // Something that is not an age file at all gets the same answer.
        assert_eq!(
            open(b"format = 1\nkind = \"full\"\n", PASSWORD).unwrap_err(),
            ArchiveError::WrongPassword
        );
        assert_eq!(
            open(b"", PASSWORD).unwrap_err(),
            ArchiveError::WrongPassword
        );
    }

    #[test]
    fn newer_format_is_refused() {
        let mut archive = sample();
        archive.format = FORMAT + 1;
        let sealed = seal(&archive, PASSWORD).unwrap();
        let err = open(&sealed, PASSWORD).unwrap_err();
        assert_eq!(err, ArchiveError::Newer);
        assert_eq!(err.to_string(), "This archive was made by a newer fbterm.");
    }

    #[test]
    fn tampered_archive_is_refused() {
        let (_, sealed) = sealed_sample();

        // A flipped byte in the encrypted payload.
        let mut body = sealed.clone();
        let last = body.len() - 1;
        body[last] ^= 0x01;
        let body_err = open(&body, PASSWORD).unwrap_err();
        assert_eq!(body_err, ArchiveError::Corrupt(DAMAGED.into()));
        assert_eq!(
            body_err.to_string(),
            "The archive is damaged or incomplete."
        );

        // A truncated file.
        let err = open(&sealed[..sealed.len() - 20], PASSWORD).unwrap_err();
        assert_eq!(err, ArchiveError::Corrupt(DAMAGED.into()));
        assert_eq!(err.to_string(), "The archive is damaged or incomplete.");

        // A flipped byte in the header MAC (checked before any scrypt work is trusted).
        let mac_start = sealed.windows(4).position(|w| w == b"\n---").unwrap() + 5;
        let mut header = sealed.clone();
        header[mac_start + 2] ^= 0x01;
        let header_err = open(&header, PASSWORD).unwrap_err();
        assert_eq!(header_err, ArchiveError::WrongPassword);

        // Errors are plain sentences without crypto jargon or secret material.
        for e in [body_err, header_err] {
            let msg = e.to_string();
            assert!(msg.ends_with('.'), "{msg}");
            for word in [
                "MAC", "age-", "scrypt", "chacha", "stream", "header", SECRET,
            ] {
                assert!(!msg.contains(word), "{msg}");
            }
        }

        // An oversized input is refused before decrypting.
        let huge = vec![0u8; MAX_ARCHIVE_BYTES + 1];
        let err = open(&huge, PASSWORD).unwrap_err();
        assert_eq!(err, ArchiveError::Corrupt(TOO_LARGE.into()));
        assert_eq!(
            err.to_string(),
            "The file is too large to be an fbterm archive."
        );
    }

    #[test]
    fn excessive_work_factor_is_refused() {
        let (_, sealed) = sealed_sample();
        // Rewrite the scrypt stanza's work factor (log2 N, the last argument) to 40.
        let start = sealed.windows(10).position(|w| w == b"-> scrypt ").unwrap();
        let end = start + sealed[start..].iter().position(|&b| b == b'\n').unwrap();
        let line = std::str::from_utf8(&sealed[start..end]).unwrap();
        let (head, _) = line.rsplit_once(' ').unwrap();
        let mut crafted = sealed[..start].to_vec();
        crafted.extend_from_slice(format!("{head} 40").as_bytes());
        crafted.extend_from_slice(&sealed[end..]);

        let err = open(&crafted, PASSWORD).unwrap_err();
        assert_eq!(err, ArchiveError::Corrupt(TOO_HARD.into()));
        assert_eq!(
            err.to_string(),
            "This archive takes too much work to open on this computer."
        );
    }

    #[test]
    fn secrets_are_zeroized_types() {
        fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>(_: &T) {}
        let archive = sample();
        wiped_on_drop(&archive.secrets[0].value);
        wiped_on_drop(&archive.keys[0].bytes);

        // Debug output never shows secret values or key bytes.
        let debug = format!("{archive:?}");
        assert!(!debug.contains(SECRET), "{debug}");
        assert!(!debug.contains("key-pass"), "{debug}");
        assert!(!debug.contains("secretkeymaterial"), "{debug}");
        assert!(debug.contains("<redacted>"));
    }
}
