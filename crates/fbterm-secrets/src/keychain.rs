//! OS keychain store: one item holding a JSON map of every profile's secrets.

use std::collections::BTreeMap;

use fbterm_config::ProfileId;
use parking_lot::Mutex;

use crate::{ProfileSecrets, SecretError, SecretKind, SecretStore};

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
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| SecretError::Backend(e.to_string()))
    }
}

impl BlobBackend for OsBlob {
    fn load(&self) -> Result<Option<String>, SecretError> {
        match self.entry()?.get_password() {
            Ok(blob) => Ok(Some(blob)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
    fn store(&self, blob: &str) -> Result<(), SecretError> {
        self.entry()?
            .set_password(blob)
            .map_err(|e| SecretError::Backend(e.to_string()))
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
}
