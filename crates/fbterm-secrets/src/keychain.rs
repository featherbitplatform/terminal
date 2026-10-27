//! OS keychain store (macOS Keychain, Linux Secret Service): one item holding a JSON map
//! of every profile's secrets. Not built on Windows, whose system storage is DPAPI.

use std::collections::BTreeMap;

use fbterm_config::ProfileId;
use parking_lot::Mutex;

use zeroize::Zeroizing;

use crate::{ProfileSecrets, SecretEntry, SecretError, SecretKind, SecretStore};

const SERVICE: &str = "fbterm";
const ACCOUNT: &str = "secrets";

/// Raw access to the single keychain item (a JSON blob).
pub(crate) trait BlobBackend: Send + Sync {
    fn load(&self) -> Result<Option<String>, SecretError>;
    fn store(&self, blob: &str) -> Result<(), SecretError>;
}

struct OsBlob {
    service: String,
    account: String,
}

impl OsBlob {
    fn entry(&self) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, &self.account).map_err(keychain_error)
    }
}

/// A keyring error as ours. Only "there is no secret service at all" is
/// [`SecretError::Unavailable`] (Auto may then move to the vault): on Linux a platform
/// failure (no D-Bus session or Secret Service) or no store at all. Denied, locked or
/// cancelled access is [`SecretError::Backend`], and so is every platform failure on macOS,
/// where a user's "Deny" arrives as one and the keychain always exists.
pub(crate) fn keychain_error(e: keyring::Error) -> SecretError {
    let text = e.to_string();
    match e {
        keyring::Error::NoDefaultStore => SecretError::Unavailable(text),
        keyring::Error::PlatformFailure(_) if !cfg!(target_os = "macos") => {
            SecretError::Unavailable(text)
        }
        _ => SecretError::Backend(text),
    }
}

impl BlobBackend for OsBlob {
    fn load(&self) -> Result<Option<String>, SecretError> {
        match self.entry()?.get_password() {
            Ok(blob) => Ok(Some(blob)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }
    fn store(&self, blob: &str) -> Result<(), SecretError> {
        self.entry()?.set_password(blob).map_err(keychain_error)
    }
}

type SecretMap = BTreeMap<String, ProfileSecrets>;

/// Secrets in the OS keychain, under the single item `("fbterm", "secrets")`.
///
/// The keychain is read once (on first access) and the map cached; every
/// write rewrites the whole item.
pub struct KeychainStore {
    backend: Box<dyn BlobBackend>,
    cache: Mutex<Option<SecretMap>>,
}

impl KeychainStore {
    pub fn new() -> Self {
        Self::with_service(SERVICE, ACCOUNT)
    }

    pub(crate) fn with_service(service: &str, account: &str) -> Self {
        Self::with_backend(Box::new(OsBlob {
            service: service.to_owned(),
            account: account.to_owned(),
        }))
    }

    pub(crate) fn with_backend(backend: Box<dyn BlobBackend>) -> Self {
        Self {
            backend,
            cache: Mutex::new(None),
        }
    }

    /// Load the cache if needed and return the guard with the map present.
    fn loaded(&self) -> Result<parking_lot::MutexGuard<'_, Option<SecretMap>>, SecretError> {
        let mut guard = self.cache.lock();
        if guard.is_none() {
            let map = match self.backend.load()? {
                Some(blob) => serde_json::from_str(&blob)
                    .map_err(|e| SecretError::Serialization(e.to_string()))?,
                None => SecretMap::new(),
            };
            *guard = Some(map);
        }
        Ok(guard)
    }

    /// Persist `map`; adopt it as the cache only if the write succeeded.
    fn commit(&self, guard: &mut Option<SecretMap>, map: SecretMap) -> Result<(), SecretError> {
        let blob =
            serde_json::to_string(&map).map_err(|e| SecretError::Serialization(e.to_string()))?;
        self.backend.store(&blob)?;
        *guard = Some(map);
        Ok(())
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        let guard = self.loaded()?;
        Ok(guard
            .as_ref()
            .and_then(|m| m.get(&id.to_string()))
            .and_then(|p| p.get(kind)))
    }

    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        let mut guard = self.loaded()?;
        let mut map = guard.clone().unwrap_or_default();
        map.entry(id.to_string()).or_default().set(kind, value);
        self.commit(&mut guard, map)
    }

    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        let mut guard = self.loaded()?;
        let mut map = guard.clone().unwrap_or_default();
        if map.remove(&id.to_string()).is_none() {
            return Ok(());
        }
        self.commit(&mut guard, map)
    }

    fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
        let mut guard = self.loaded()?;
        let mut map = guard.clone().unwrap_or_default();
        let key = id.to_string();
        let Some(secrets) = map.get_mut(&key) else {
            return Ok(());
        };
        if !secrets.clear(kind) {
            return Ok(());
        }
        if secrets.is_empty() {
            map.remove(&key);
        }
        self.commit(&mut guard, map)
    }

    fn entries(&self, ids: &[ProfileId]) -> Result<Vec<SecretEntry>, SecretError> {
        let guard = self.loaded()?;
        let mut out = Vec::new();
        let Some(map) = guard.as_ref() else {
            return Ok(out);
        };
        for id in ids {
            if let Some(secrets) = map.get(&id.to_string()) {
                for kind in SecretKind::ALL {
                    if let Some(value) = secrets.get(kind) {
                        out.push((*id, kind, Zeroizing::new(value)));
                    }
                }
            }
        }
        Ok(out)
    }

    /// All of `items` in one keychain write.
    fn set_many(&self, items: &[SecretEntry]) -> Result<(), SecretError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut guard = self.loaded()?;
        let mut map = guard.clone().unwrap_or_default();
        for (id, kind, value) in items {
            map.entry(id.to_string()).or_default().set(*kind, value);
        }
        self.commit(&mut guard, map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(msg: &str) -> keyring::Error {
        keyring::Error::PlatformFailure(Box::new(std::io::Error::other(msg.to_owned())))
    }

    #[test]
    fn only_a_missing_service_is_unavailable() {
        // Denied, locked or cancelled access: the storage is there.
        let denied = keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("locked")));
        assert!(matches!(keychain_error(denied), SecretError::Backend(_)));
        assert!(matches!(
            keychain_error(keyring::Error::NoDefaultStore),
            SecretError::Unavailable(_)
        ));
        let failure = keychain_error(platform("no secret service"));
        if cfg!(target_os = "macos") {
            // macOS reports a user's "Deny" as a platform failure, and always has a keychain.
            assert!(matches!(failure, SecretError::Backend(_)), "{failure:?}");
        } else {
            assert!(
                matches!(failure, SecretError::Unavailable(_)),
                "{failure:?}"
            );
        }
    }
}
