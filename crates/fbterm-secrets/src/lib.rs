//! Keychain-backed secret storage.
//!
//! Saved SSH passwords and key passphrases live here, never in config files.
//! Secret values are never logged.

mod keychain;

pub use keychain::KeychainStore;

use std::sync::atomic::{AtomicBool, Ordering};

use fbterm_config::ProfileId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    Password,
    KeyPassphrase,
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain error: {0}")]
    Backend(String),
    #[error("stored secret map is invalid: {0}")]
    Serialization(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError>;
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError>;
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError>;
}

/// The secrets of one profile; also the JSON shape stored per profile.
#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct ProfileSecrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_passphrase: Option<String>,
}

impl ProfileSecrets {
    fn slot(&mut self, kind: SecretKind) -> &mut Option<String> {
        match kind {
            SecretKind::Password => &mut self.password,
            SecretKind::KeyPassphrase => &mut self.key_passphrase,
        }
    }
    pub(crate) fn get(&self, kind: SecretKind) -> Option<String> {
        match kind {
            SecretKind::Password => self.password.clone(),
            SecretKind::KeyPassphrase => self.key_passphrase.clone(),
        }
    }
    pub(crate) fn set(&mut self, kind: SecretKind, value: &str) {
        *self.slot(kind) = Some(value.to_owned());
    }
}

/// In-memory store: used by tests and as the session fallback.
#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<ProfileId, ProfileSecrets>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        Ok(self.map.lock().get(id).and_then(|p| p.get(kind)))
    }
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        self.map.lock().entry(*id).or_default().set(kind, value);
        Ok(())
    }
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        self.map.lock().remove(id);
        Ok(())
    }
}

/// Keychain first; after the first keychain error everything goes to a
/// session-only in-memory store (the keychain is not retried).
pub struct FallbackStore {
    primary: Box<dyn SecretStore>,
    session: MemoryStore,
    degraded: AtomicBool,
}

impl FallbackStore {
    pub fn new() -> Self {
        Self::with_primary(Box::new(KeychainStore::new()))
    }

    pub(crate) fn with_primary(primary: Box<dyn SecretStore>) -> Self {
        Self {
            primary,
            session: MemoryStore::new(),
            degraded: AtomicBool::new(false),
        }
    }

    /// True once the keychain failed and secrets are session-only.
    pub fn degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }

    fn degrade(&self, err: &SecretError) {
        if !self.degraded.swap(true, Ordering::SeqCst) {
            tracing::warn!(error = %err, "keychain unavailable; keeping secrets for this session only");
        }
    }

    /// Run `op` on the primary unless degraded; on error degrade and return None.
    fn try_primary<T>(
        &self,
        op: impl FnOnce(&dyn SecretStore) -> Result<T, SecretError>,
    ) -> Option<T> {
        if self.degraded() {
            return None;
        }
        match op(self.primary.as_ref()) {
            Ok(v) => Some(v),
            Err(e) => {
                self.degrade(&e);
                None
            }
        }
    }
}

impl Default for FallbackStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for FallbackStore {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        match self.try_primary(|p| p.get(id, kind)) {
            Some(v) => Ok(v),
            None => self.session.get(id, kind),
        }
    }
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        match self.try_primary(|p| p.set(id, kind, value)) {
            Some(()) => Ok(()),
            None => self.session.set(id, kind, value),
        }
    }
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        match self.try_primary(|p| p.remove(id)) {
            Some(()) => Ok(()),
            None => self.session.remove(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keychain::BlobBackend;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pid() -> ProfileId {
        ProfileId(uuid::Uuid::new_v4())
    }

    #[derive(Default)]
    struct MockBlob {
        blob: Mutex<Option<String>>,
        loads: AtomicUsize,
        stores: AtomicUsize,
    }
    impl BlobBackend for Arc<MockBlob> {
        fn load(&self) -> Result<Option<String>, SecretError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(self.blob.lock().clone())
        }
        fn store(&self, blob: &str) -> Result<(), SecretError> {
            self.stores.fetch_add(1, Ordering::SeqCst);
            *self.blob.lock() = Some(blob.to_owned());
            Ok(())
        }
    }

    struct FailingStore;
    impl SecretStore for FailingStore {
        fn get(&self, _: &ProfileId, _: SecretKind) -> Result<Option<String>, SecretError> {
            Err(SecretError::Backend("denied".into()))
        }
        fn set(&self, _: &ProfileId, _: SecretKind, _: &str) -> Result<(), SecretError> {
            Err(SecretError::Backend("denied".into()))
        }
        fn remove(&self, _: &ProfileId) -> Result<(), SecretError> {
            Err(SecretError::Backend("denied".into()))
        }
    }

    #[test]
    fn memory_store_set_get_remove() {
        let s = MemoryStore::new();
        let id = pid();
        assert_eq!(s.get(&id, SecretKind::Password).unwrap(), None);
        s.set(&id, SecretKind::Password, "pw").unwrap();
        s.set(&id, SecretKind::KeyPassphrase, "pp").unwrap();
        assert_eq!(
            s.get(&id, SecretKind::Password).unwrap().as_deref(),
            Some("pw")
        );
        assert_eq!(
            s.get(&id, SecretKind::KeyPassphrase).unwrap().as_deref(),
            Some("pp")
        );
        s.remove(&id).unwrap();
        assert_eq!(s.get(&id, SecretKind::Password).unwrap(), None);
        assert_eq!(s.get(&id, SecretKind::KeyPassphrase).unwrap(), None);
    }

    #[test]
    fn map_serialization_has_one_entry_for_many_profiles() {
        let mock = Arc::new(MockBlob::default());
        let s = KeychainStore::with_backend(Box::new(mock.clone()));
        let ids: Vec<_> = (0..20).map(|_| pid()).collect();
        for id in &ids {
            s.set(id, SecretKind::Password, "pw").unwrap();
        }
        s.set(&ids[0], SecretKind::KeyPassphrase, "pp").unwrap();
        let blob = mock.blob.lock().clone().unwrap();
        let v: serde_json::Value = serde_json::from_str(&blob).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 20);
        let first = &v[ids[0].to_string()];
        assert_eq!(first["password"], "pw");
        assert_eq!(first["key_passphrase"], "pp");
        assert!(v[ids[1].to_string()].get("key_passphrase").is_none());
    }

    #[test]
    fn keychain_reads_once_and_prunes_empty_profiles() {
        let mock = Arc::new(MockBlob::default());
        let s = KeychainStore::with_backend(Box::new(mock.clone()));
        let (a, b) = (pid(), pid());
        s.set(&a, SecretKind::Password, "x").unwrap();
        s.set(&b, SecretKind::Password, "y").unwrap();
        assert_eq!(
            s.get(&a, SecretKind::Password).unwrap().as_deref(),
            Some("x")
        );
        assert_eq!(mock.loads.load(Ordering::SeqCst), 1);
        s.remove(&a).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&mock.blob.lock().clone().unwrap()).unwrap();
        assert!(v.get(a.to_string()).is_none());
        assert!(v.get(b.to_string()).is_some());
        // removing an unknown profile does not rewrite the item
        let stores = mock.stores.load(Ordering::SeqCst);
        s.remove(&a).unwrap();
        assert_eq!(mock.stores.load(Ordering::SeqCst), stores);
    }

    #[test]
    fn keychain_loads_existing_blob() {
        let id = pid();
        let mock = Arc::new(MockBlob::default());
        *mock.blob.lock() = Some(format!(r#"{{"{id}":{{"password":"old"}}}}"#));
        let s = KeychainStore::with_backend(Box::new(mock));
        assert_eq!(
            s.get(&id, SecretKind::Password).unwrap().as_deref(),
            Some("old")
        );
    }

    #[test]
    fn keychain_corrupt_blob_is_serialization_error() {
        let mock = Arc::new(MockBlob::default());
        *mock.blob.lock() = Some("not json".into());
        let s = KeychainStore::with_backend(Box::new(mock));
        assert!(matches!(
            s.get(&pid(), SecretKind::Password),
            Err(SecretError::Serialization(_))
        ));
    }

    #[test]
    fn fallback_serves_session_secrets_when_primary_fails() {
        let s = FallbackStore::with_primary(Box::new(FailingStore));
        let id = pid();
        assert!(!s.degraded());
        s.set(&id, SecretKind::Password, "pw").unwrap();
        assert!(s.degraded());
        assert_eq!(
            s.get(&id, SecretKind::Password).unwrap().as_deref(),
            Some("pw")
        );
        s.remove(&id).unwrap();
        assert_eq!(s.get(&id, SecretKind::Password).unwrap(), None);
    }

    #[test]
    #[ignore = "touches the real OS keychain"]
    fn keychain_round_trip() {
        let entry = keyring::Entry::new("fbterm-test", "secrets").unwrap();
        let _ = entry.delete_credential();
        let s = KeychainStore::with_service("fbterm-test", "secrets");
        let id = pid();
        s.set(&id, SecretKind::Password, "hunter2").unwrap();
        // a fresh store (empty cache) must see what was persisted
        let s2 = KeychainStore::with_service("fbterm-test", "secrets");
        assert_eq!(
            s2.get(&id, SecretKind::Password).unwrap().as_deref(),
            Some("hunter2")
        );
        s2.remove(&id).unwrap();
        let s3 = KeychainStore::with_service("fbterm-test", "secrets");
        assert_eq!(s3.get(&id, SecretKind::Password).unwrap(), None);
        let _ = entry.delete_credential();
    }
}
