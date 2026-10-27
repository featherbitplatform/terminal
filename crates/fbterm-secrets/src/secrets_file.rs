//! What the file-backed stores (the vault and, on Windows, the DPAPI file) share: the
//! versioned TOML document that holds every saved secret, bounded reads and atomic writes.
//!
//! Callers encrypt the document before it reaches the disk and map the errors here to their
//! own plain sentences.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use fbterm_config::ProfileId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::SecretKind;
use crate::archive::ArchivedSecret;

/// Every saved secret, by profile and kind. Values are wiped on drop.
pub(crate) type Contents = BTreeMap<(ProfileId, SecretKind), Zeroizing<String>>;

/// The document: `format = N`, then the secrets.
#[derive(Serialize, Deserialize)]
struct Document {
    format: u32,
    #[serde(default)]
    secrets: Vec<ArchivedSecret>,
}

/// Why a decrypted document could not be read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// Not text, not TOML, no usable `format`, or a malformed secret.
    Unreadable,
    /// A `format` newer than the caller reads.
    Newer,
}

/// Why a file could not be read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadError {
    Missing,
    TooLarge,
    Io,
}

/// The document for `contents` at `format`, in a wiped buffer. Serialization of these
/// plain types does not fail in practice; `None` if it ever does.
pub(crate) fn to_toml(contents: &Contents, format: u32) -> Option<Zeroizing<String>> {
    let doc = Document {
        format,
        secrets: contents
            .iter()
            .map(|((profile, kind), value)| ArchivedSecret {
                profile: *profile,
                kind: *kind,
                value: value.clone(),
            })
            .collect(),
    };
    toml::to_string(&doc).ok().map(Zeroizing::new)
}

/// Parse a decrypted document whose `format` may be at most `newest`. Parser messages
/// quote the offending line, which may hold a secret: they are never passed on.
pub(crate) fn parse(plain: &[u8], newest: u32) -> Result<Contents, ParseError> {
    let text = std::str::from_utf8(plain).map_err(|_| ParseError::Unreadable)?;
    // The version first, so a newer layout is reported as such rather than as unreadable.
    #[derive(Deserialize)]
    struct Version {
        format: Option<u32>,
    }
    let version: Version = toml::from_str(text).map_err(|_| ParseError::Unreadable)?;
    match version.format {
        Some(f) if f > newest => return Err(ParseError::Newer),
        Some(f) if f >= 1 => {}
        _ => return Err(ParseError::Unreadable),
    }
    let doc: Document = toml::from_str(text).map_err(|_| ParseError::Unreadable)?;
    Ok(doc
        .secrets
        .into_iter()
        .map(|s| ((s.profile, s.kind), s.value))
        .collect())
}

/// Read the file at `path`, refusing anything larger than `max` bytes. The bytes are
/// ciphertext, so they need no wiping.
pub(crate) fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, ReadError> {
    let file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ReadError::Missing,
        _ => ReadError::Io,
    })?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::Io)?;
    if bytes.len() as u64 > max {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

/// Write `bytes` to a new part file (mode 0600 on Unix) next to `path`, sync it, rename it
/// over `path` and sync the directory where the platform allows. The part file is removed
/// on failure.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let part = dir.join(format!(".{name}.{}.part", ProfileId::new().0));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&part)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    // Make the rename itself durable. Not possible on every platform (Windows cannot open a
    // directory this way), so a failure here is not an error.
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}
