//! Secret storage and encrypted profile archives.
//!
//! Saved SSH passwords and key passphrases live here, never in config files: in the
//! system's secure storage (the OS keychain on macOS and Linux, a DPAPI-protected file on
//! Windows) or in the encrypted vault. Secret values are never logged.

pub mod archive;
#[cfg(any(windows, test))]
mod dpapi;
#[cfg(not(windows))]
mod keychain;
mod secrets_file;
pub mod vault;

#[cfg(windows)]
pub use dpapi::DpapiStore;
#[cfg(not(windows))]
pub use keychain::KeychainStore;
pub use vault::VaultStore;
pub use zeroize::Zeroizing;

use std::path::{Path, PathBuf};

use fbterm_config::{Paths, ProfileId, SecretStorage};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    Password,
    KeyPassphrase,
}

impl SecretKind {
    pub const ALL: [SecretKind; 2] = [SecretKind::Password, SecretKind::KeyPassphrase];
}

/// One saved secret: the profile it belongs to, its kind and its value (wiped on drop).
pub type SecretEntry = (ProfileId, SecretKind, Zeroizing<String>);

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain error: {0}")]
    Backend(String),
    #[error("stored secret map is invalid: {0}")]
    Serialization(String),
    /// The vault has not been unlocked in this session (or was locked again).
    #[error("Saved secrets are locked.")]
    Locked,
    /// The main password does not open the vault.
    #[error("Wrong main password.")]
    WrongMainPassword,
    /// `create` found a vault file already in place and left it alone.
    #[error("An encrypted vault already exists.")]
    VaultExists,
    /// The vault file could not be read or written: a plain sentence for the user, never
    /// secret material or raw parser output.
    #[error("{0}")]
    Vault(String),
    /// The system storage file (DPAPI on Windows) could not be read or written: a plain
    /// sentence for the user, never secret material or raw parser output.
    #[error("{0}")]
    Storage(String),
    /// Saved secrets use the encrypted vault, and it has no main password yet.
    #[error("Saved secrets need a main password.")]
    NoMainPassword,
    /// A storage switch copied the secrets but did not read the same ones back: nothing was
    /// removed from the old storage.
    #[error("The copied secrets did not read back the same.")]
    NotVerified,
}

pub trait SecretStore: Send + Sync {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError>;
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError>;
    /// Forget every secret of a profile.
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError>;
    /// Forget one kind of secret of a profile (its other kind is kept).
    fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError>;

    /// Every saved secret of the given profiles. Stores that cannot be enumerated (the
    /// keychain) are asked by id, so callers pass the ids from the profile store.
    fn entries(&self, ids: &[ProfileId]) -> Result<Vec<SecretEntry>, SecretError> {
        let mut out = Vec::new();
        for id in ids {
            for kind in SecretKind::ALL {
                if let Some(value) = self.get(id, kind)? {
                    out.push((*id, kind, Zeroizing::new(value)));
                }
            }
        }
        Ok(out)
    }

    /// Save several secrets. The default sets them one by one and is NOT atomic: an error
    /// part way leaves the earlier ones saved. Stores whose every write is expensive (the
    /// vault re-encrypts its whole file) override it with one write, which is all or nothing.
    fn set_many(&self, items: &[SecretEntry]) -> Result<(), SecretError> {
        for (id, kind, value) in items {
            self.set(id, *kind, value)?;
        }
        Ok(())
    }

    /// Forget every secret of these profiles. Like [`Self::set_many`], the default removes
    /// them one by one and is not atomic; the vault overrides it with one write.
    fn remove_many(&self, ids: &[ProfileId]) -> Result<(), SecretError> {
        for id in ids {
            self.remove(id)?;
        }
        Ok(())
    }

    /// Open a store that needs the user's main password (the encrypted vault). Stores
    /// without one have nothing to open and accept anything. Runs scrypt for the vault:
    /// never call it on the UI thread.
    fn unlock_vault(&self, _main_password: &str) -> Result<(), SecretError> {
        Ok(())
    }
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
    /// Clear one kind; returns whether there was something to clear.
    pub(crate) fn clear(&mut self, kind: SecretKind) -> bool {
        self.slot(kind).take().is_some()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.password.is_none() && self.key_passphrase.is_none()
    }
}

/// In-memory store: used by tests and by `FBTERM_SECRETS=memory`.
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
    fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
        let mut map = self.map.lock();
        if let Some(p) = map.get_mut(id) {
            p.clear(kind);
            if p.is_empty() {
                map.remove(id);
            }
        }
        Ok(())
    }
}

/// The system's secure storage: the OS keychain (macOS Keychain, Linux Secret Service), or
/// on Windows the DPAPI-protected file `<config>/secrets.dpapi`.
pub fn system_store(paths: &Paths) -> Box<dyn SecretStore> {
    #[cfg(windows)]
    {
        Box::new(DpapiStore::new(paths.dpapi()))
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Box::new(KeychainStore::new())
    }
}

/// Which storage holds saved secrets now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// The system's secure storage ([`system_store`]).
    System,
    /// The encrypted vault (`<config>/secrets.fbvault`).
    Vault,
}

impl Storage {
    /// The storage a `secret_storage` setting starts with (Auto starts with the system's and
    /// may move to the vault once, see [`SecretBackend::detect_auto`]).
    pub fn for_setting(setting: SecretStorage) -> Self {
        match setting {
            SecretStorage::Auto | SecretStorage::Keychain => Self::System,
            SecretStorage::Vault => Self::Vault,
        }
    }
}

/// What the vault in use needs before secrets can be saved or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainPassword {
    /// The vault exists and is locked: `Unlock saved secrets`.
    Unlock,
    /// No vault yet: `Set a main password`.
    Set,
}

/// The app's secret store: the system's storage or the encrypted vault, whichever the
/// `secret_storage` setting chose, behind one [`SecretStore`].
///
/// Every call runs against the storage in use under a read lock, and a switch holds the write
/// lock for the whole move, so a call in flight finishes before a switch starts and a call
/// made during one waits for it. Vault calls run scrypt or wait on a write that does: callers
/// keep every call off the UI thread.
///
/// With the vault in use but locked, calls fail with [`SecretError::Locked`]; with no vault
/// file yet, reads find nothing and saves fail with [`SecretError::NoMainPassword`].
pub struct SecretBackend {
    active: RwLock<Storage>,
    system: Box<dyn SecretStore>,
    vault: VaultStore,
    vault_path: PathBuf,
    /// The setting at startup was `"auto"`: [`Self::detect_auto`] may move to the vault.
    auto: bool,
    /// The memory backend's temporary folder (holding its vault), removed on drop.
    temp_dir: Option<PathBuf>,
}

impl SecretBackend {
    /// The backend for `setting` over the system storage and vault of `paths`. Nothing is
    /// read until the first call.
    pub fn new(setting: SecretStorage, paths: &Paths) -> Self {
        Self::with_system(setting, system_store(paths), paths.vault())
    }

    /// The backend of `FBTERM_SECRETS=memory` and tests: an in-memory system storage and a
    /// vault in a fresh temporary folder (removed when the backend drops), so switching works
    /// without touching the real keychain, DPAPI file or config folder. `setting` only picks
    /// the storage to start with (Auto never moves: the memory store always works).
    pub fn in_memory(setting: SecretStorage) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fbterm-memory-secrets-{}-{}",
            std::process::id(),
            next_temp()
        ));
        let setting = match setting {
            SecretStorage::Auto => SecretStorage::Keychain,
            other => other,
        };
        let mut backend = Self::with_system(
            setting,
            Box::new(MemoryStore::new()),
            dir.join("secrets.fbvault"),
        );
        backend.temp_dir = Some(dir);
        backend
    }

    /// The backend over any system store, with its vault at `vault_path`.
    pub fn with_system(
        setting: SecretStorage,
        system: Box<dyn SecretStore>,
        vault_path: PathBuf,
    ) -> Self {
        Self {
            active: RwLock::new(Storage::for_setting(setting)),
            system,
            vault: VaultStore::new(),
            vault_path,
            auto: setting == SecretStorage::Auto,
            temp_dir: None,
        }
    }

    pub fn active(&self) -> Storage {
        *self.active.read()
    }

    pub fn vault_path(&self) -> &Path {
        &self.vault_path
    }

    pub fn vault_exists(&self) -> bool {
        VaultStore::exists(&self.vault_path)
    }

    pub fn vault_unlocked(&self) -> bool {
        self.vault.is_unlocked()
    }

    /// Whether this is the [`Self::in_memory`] backend (its vault in a temporary folder).
    pub fn is_in_memory(&self) -> bool {
        self.temp_dir.is_some()
    }

    /// What the storage in use needs before it can save: `None` for the system storage or an
    /// unlocked vault.
    pub fn needs_main_password(&self) -> Option<MainPassword> {
        if self.active() == Storage::System || self.vault.is_unlocked() {
            None
        } else if self.vault_exists() {
            Some(MainPassword::Unlock)
        } else {
            Some(MainPassword::Set)
        }
    }

    /// Auto at startup: when the system storage does not work at all (no secret service on
    /// Linux, say), use the vault from now on. Probes once with one read; returns whether it
    /// switched (the app then writes `"vault"` and says why once). Not for the UI thread.
    pub fn detect_auto(&self) -> bool {
        let mut active = self.active.write();
        if !self.auto || *active != Storage::System {
            return false;
        }
        // Only "not there at all" counts: a damaged or unreadable item is still the user's
        // storage, and moving away from it would hide those secrets.
        // One read of an id nothing has: every store reads its item or file for it.
        match self.system.get(&ProfileId::new(), SecretKind::Password) {
            Err(SecretError::Backend(e)) => {
                tracing::warn!(error = %e, "system secret storage unavailable; using the encrypted vault");
                *active = Storage::Vault;
                true
            }
            _ => false,
        }
    }

    /// Make the vault with `main_password` and leave it unlocked (no vault may exist yet).
    pub fn create_vault(&self, main_password: &str) -> Result<(), SecretError> {
        self.vault.create(&self.vault_path, main_password)
    }

    /// `Reset vault`: delete the vault file and forget its contents and main password. The
    /// storage in use stays as it is: with the vault in use, the next save asks for a new
    /// main password.
    pub fn reset_vault(&self) -> Result<(), SecretError> {
        // No call is in flight on the vault while it goes.
        let _active = self.active.write();
        self.vault.reset(&self.vault_path)
    }

    /// The store behind `storage`.
    fn store(&self, storage: Storage) -> &dyn SecretStore {
        match storage {
            Storage::System => self.system.as_ref(),
            Storage::Vault => &self.vault,
        }
    }

    /// Make the vault usable for a switch: unlocked, created with `main_password` when there
    /// is none and `create` allows it.
    fn open_vault(&self, main_password: Option<&str>, create: bool) -> Result<(), SecretError> {
        if self.vault.is_unlocked() {
            return Ok(());
        }
        let exists = self.vault_exists();
        match main_password {
            None if exists => Err(SecretError::Locked),
            None => Err(SecretError::NoMainPassword),
            Some(pw) if exists => self.vault.unlock(&self.vault_path, pw),
            Some(pw) if create => self.vault.create(&self.vault_path, pw),
            // Moving away from a vault that does not exist: nothing to read.
            Some(_) => Ok(()),
        }
    }

    /// Run `op` on the storage in use (see the type's notes on a vault with no file).
    fn with_active<T>(
        &self,
        missing: impl FnOnce() -> Result<T, SecretError>,
        op: impl FnOnce(&dyn SecretStore) -> Result<T, SecretError>,
    ) -> Result<T, SecretError> {
        let active = self.active.read();
        if *active == Storage::Vault && !self.vault.is_unlocked() && !self.vault_exists() {
            return missing();
        }
        op(self.store(*active))
    }

    /// Move every saved secret of `ids` to `target` and use it from now on.
    ///
    /// The secrets are read from the storage in use, written to `target`, read back and
    /// compared, and only then removed from the old storage. A failed read of the old
    /// storage, a failed write or a different read-back cancels the switch: the old storage
    /// keeps everything and stays in use. Moving to the vault creates it with
    /// `main_password` when there is none, else unlocks it; moving away from a locked vault
    /// unlocks it first. A failure to clear the old storage after the move is logged (the
    /// secrets are safe in the new one).
    pub fn switch_to(
        &self,
        target: Storage,
        main_password: Option<&str>,
        ids: &[ProfileId],
    ) -> Result<(), SecretError> {
        let mut active = self.active.write();
        let from = *active;
        if from == target {
            return Ok(());
        }
        // The vault's side first: reading from a locked vault needs it unlocked, and writing
        // to the vault needs it made or unlocked. A vault being left that was never made
        // holds nothing.
        let old_vault_missing = from == Storage::Vault && !self.vault_exists();
        if from == Storage::Vault && !old_vault_missing {
            self.open_vault(main_password, false)?;
        }
        let old = self.store(from);
        let moving = if old_vault_missing {
            Vec::new()
        } else {
            old.entries(ids)?
        };
        if target == Storage::Vault {
            self.open_vault(main_password, true)?;
        }
        let new = self.store(target);
        new.set_many(&moving)?;
        let found: HashMap<(ProfileId, SecretKind), Zeroizing<String>> = new
            .entries(ids)?
            .into_iter()
            .map(|(id, kind, value)| ((id, kind), value))
            .collect();
        let verified = moving
            .iter()
            .all(|(id, kind, value)| found.get(&(*id, *kind)).is_some_and(|v| v == value));
        if !verified {
            return Err(SecretError::NotVerified);
        }
        *active = target;
        tracing::info!(?target, moved = moving.len(), "saved secrets moved");
        let mut holders: Vec<ProfileId> = moving.iter().map(|(id, _, _)| *id).collect();
        holders.sort();
        holders.dedup();
        if let Err(e) = old.remove_many(&holders) {
            tracing::warn!(error = %e, "moved secrets could not be removed from the old storage");
        }
        Ok(())
    }
}

impl Drop for SecretBackend {
    fn drop(&mut self) {
        if let Some(dir) = &self.temp_dir {
            self.vault.lock();
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Numbers the memory backends' temporary folders within this process.
fn next_temp() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl SecretStore for SecretBackend {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        self.with_active(|| Ok(None), |s| s.get(id, kind))
    }
    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        self.with_active(
            || Err(SecretError::NoMainPassword),
            |s| s.set(id, kind, value),
        )
    }
    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        self.with_active(|| Ok(()), |s| s.remove(id))
    }
    fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
        self.with_active(|| Ok(()), |s| s.remove_kind(id, kind))
    }
    fn entries(&self, ids: &[ProfileId]) -> Result<Vec<SecretEntry>, SecretError> {
        self.with_active(|| Ok(Vec::new()), |s| s.entries(ids))
    }
    fn set_many(&self, items: &[SecretEntry]) -> Result<(), SecretError> {
        self.with_active(|| Err(SecretError::NoMainPassword), |s| s.set_many(items))
    }
    fn remove_many(&self, ids: &[ProfileId]) -> Result<(), SecretError> {
        self.with_active(|| Ok(()), |s| s.remove_many(ids))
    }

    /// Unlock the vault (whether or not it is in use). An open vault accepts any answer.
    fn unlock_vault(&self, main_password: &str) -> Result<(), SecretError> {
        if self.vault.is_unlocked() {
            return Ok(());
        }
        self.vault.unlock(&self.vault_path, main_password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid() -> ProfileId {
        ProfileId(uuid::Uuid::new_v4())
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
    fn memory_store_remove_kind_keeps_the_other_kind() {
        let s = MemoryStore::new();
        let id = pid();
        s.set(&id, SecretKind::Password, "pw").unwrap();
        s.set(&id, SecretKind::KeyPassphrase, "pp").unwrap();
        s.remove_kind(&id, SecretKind::Password).unwrap();
        assert_eq!(s.get(&id, SecretKind::Password).unwrap(), None);
        assert_eq!(
            s.get(&id, SecretKind::KeyPassphrase).unwrap().as_deref(),
            Some("pp")
        );
        s.remove_kind(&id, SecretKind::KeyPassphrase).unwrap();
        assert!(s.map.lock().is_empty(), "an empty profile entry is pruned");
        s.remove_kind(&pid(), SecretKind::Password).unwrap();
    }

    fn entry(id: ProfileId, kind: SecretKind, value: &str) -> SecretEntry {
        (id, kind, Zeroizing::new(value.to_owned()))
    }

    fn plain(entries: Vec<SecretEntry>) -> Vec<(ProfileId, SecretKind, String)> {
        let mut v: Vec<_> = entries
            .into_iter()
            .map(|(id, kind, value)| (id, kind, value.as_str().to_owned()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn memory_store_entries_and_set_many() {
        let s = MemoryStore::new();
        let (a, b) = (pid(), pid());
        let items = vec![
            entry(a, SecretKind::KeyPassphrase, "pp"),
            entry(b, SecretKind::Password, "pw"),
        ];
        s.set_many(&items).unwrap();
        assert_eq!(plain(s.entries(&[a, b, pid()]).unwrap()), plain(items));
        assert!(s.entries(&[]).unwrap().is_empty());
    }

    /// The switcher between the system storage and the vault, over the fake DPAPI codec
    /// (or an in-memory store) and a vault in a temporary folder.
    mod backend {
        use super::*;
        use crate::dpapi::CodecStore;
        use crate::dpapi::test_codec::Fake;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        const MAIN: &str = "main password for backend tests";

        /// A shared view of a system store, so a test can look at it after handing it over.
        #[derive(Clone)]
        struct Shared(Arc<dyn SecretStore>);
        impl SecretStore for Shared {
            fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
                self.0.get(id, kind)
            }
            fn set(
                &self,
                id: &ProfileId,
                kind: SecretKind,
                value: &str,
            ) -> Result<(), SecretError> {
                self.0.set(id, kind, value)
            }
            fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
                self.0.remove(id)
            }
            fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
                self.0.remove_kind(id, kind)
            }
            fn entries(&self, ids: &[ProfileId]) -> Result<Vec<SecretEntry>, SecretError> {
                self.0.entries(ids)
            }
            fn set_many(&self, items: &[SecretEntry]) -> Result<(), SecretError> {
                self.0.set_many(items)
            }
        }

        /// A system storage that is not there at all (no secret service).
        struct Unavailable;
        impl SecretStore for Unavailable {
            fn get(&self, _: &ProfileId, _: SecretKind) -> Result<Option<String>, SecretError> {
                Err(SecretError::Backend("no secret service".into()))
            }
            fn set(&self, _: &ProfileId, _: SecretKind, _: &str) -> Result<(), SecretError> {
                Err(SecretError::Backend("no secret service".into()))
            }
            fn remove(&self, _: &ProfileId) -> Result<(), SecretError> {
                Err(SecretError::Backend("no secret service".into()))
            }
            fn remove_kind(&self, _: &ProfileId, _: SecretKind) -> Result<(), SecretError> {
                Err(SecretError::Backend("no secret service".into()))
            }
        }

        /// A store over `inner` whose reads fail while `fail_reads` is set, and which
        /// counts removals and can drop writes silently.
        #[derive(Default)]
        struct Flaky {
            inner: MemoryStore,
            fail_reads: AtomicBool,
            fail_writes: AtomicBool,
            lose_writes: AtomicBool,
            removals: AtomicUsize,
        }
        impl Flaky {
            fn read(&self) -> Result<(), SecretError> {
                if self.fail_reads.load(Ordering::SeqCst) {
                    return Err(SecretError::Backend("read denied".into()));
                }
                Ok(())
            }
            fn write(&self) -> Result<bool, SecretError> {
                if self.fail_writes.load(Ordering::SeqCst) {
                    return Err(SecretError::Backend("write denied".into()));
                }
                Ok(!self.lose_writes.load(Ordering::SeqCst))
            }
        }
        impl SecretStore for Flaky {
            fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
                self.read()?;
                self.inner.get(id, kind)
            }
            fn set(
                &self,
                id: &ProfileId,
                kind: SecretKind,
                value: &str,
            ) -> Result<(), SecretError> {
                if self.write()? {
                    self.inner.set(id, kind, value)?;
                }
                Ok(())
            }
            fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
                self.removals.fetch_add(1, Ordering::SeqCst);
                self.inner.remove(id)
            }
            fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
                self.removals.fetch_add(1, Ordering::SeqCst);
                self.inner.remove_kind(id, kind)
            }
        }

        fn dpapi_store(dir: &Path) -> Arc<dyn SecretStore> {
            let path = Paths {
                config_dir: dir.to_path_buf(),
            }
            .dpapi();
            Arc::new(CodecStore::with_codec(path, Fake::default()))
        }

        fn backend(
            setting: SecretStorage,
            system: Arc<dyn SecretStore>,
            dir: &Path,
        ) -> SecretBackend {
            let paths = Paths {
                config_dir: dir.to_path_buf(),
            };
            SecretBackend::with_system(setting, Box::new(Shared(system)), paths.vault())
        }

        /// Two profiles' secrets: a password, and a password plus a passphrase.
        fn sample() -> (Vec<ProfileId>, Vec<SecretEntry>) {
            let (a, b) = (pid(), pid());
            let items = vec![
                entry(a, SecretKind::Password, "pw-a-5c1e"),
                entry(b, SecretKind::Password, "pw-b-\u{e9}"),
                entry(b, SecretKind::KeyPassphrase, "pp-b-77d0"),
            ];
            (vec![a, b, pid()], items)
        }

        #[test]
        fn auto_uses_keychain_when_it_works() {
            let dir = tempfile::tempdir().unwrap();
            let system = dpapi_store(dir.path());
            let b = backend(SecretStorage::Auto, system.clone(), dir.path());
            assert!(!b.detect_auto(), "the system storage works");
            assert_eq!(b.active(), Storage::System);
            assert_eq!(b.needs_main_password(), None);
            let id = pid();
            b.set(&id, SecretKind::Password, "pw").unwrap();
            assert_eq!(
                system.get(&id, SecretKind::Password).unwrap().as_deref(),
                Some("pw")
            );
            assert_eq!(
                b.get(&id, SecretKind::Password).unwrap().as_deref(),
                Some("pw")
            );
            assert!(!b.vault_exists(), "no vault is made");
        }

        #[test]
        fn auto_switches_when_keychain_unavailable() {
            let dir = tempfile::tempdir().unwrap();
            let b = backend(SecretStorage::Auto, Arc::new(Unavailable), dir.path());
            assert!(b.detect_auto());
            assert_eq!(b.active(), Storage::Vault);
            // No vault yet: nothing is saved, and a save needs a main password first.
            assert_eq!(b.needs_main_password(), Some(MainPassword::Set));
            let id = pid();
            assert_eq!(b.get(&id, SecretKind::Password).unwrap(), None);
            assert!(b.entries(&[id]).unwrap().is_empty());
            b.remove(&id).unwrap();
            assert!(matches!(
                b.set(&id, SecretKind::Password, "pw"),
                Err(SecretError::NoMainPassword)
            ));
            assert!(!b.detect_auto(), "probed once: already switched");

            b.create_vault(MAIN).unwrap();
            assert_eq!(b.needs_main_password(), None);
            b.set(&id, SecretKind::Password, "pw").unwrap();
            assert_eq!(
                b.get(&id, SecretKind::Password).unwrap().as_deref(),
                Some("pw")
            );

            // An explicit keychain choice is kept even when the keychain fails.
            let other = tempfile::tempdir().unwrap();
            let b = backend(SecretStorage::Keychain, Arc::new(Unavailable), other.path());
            assert!(!b.detect_auto());
            assert_eq!(b.active(), Storage::System);
        }

        #[test]
        fn switch_to_vault_moves_everything() {
            let dir = tempfile::tempdir().unwrap();
            let system = dpapi_store(dir.path());
            let (ids, items) = sample();
            system.set_many(&items).unwrap();
            let b = backend(SecretStorage::Keychain, system.clone(), dir.path());

            assert!(matches!(
                b.switch_to(Storage::Vault, None, &ids),
                Err(SecretError::NoMainPassword)
            ));
            assert_eq!(b.active(), Storage::System);
            assert_eq!(plain(system.entries(&ids).unwrap()), plain(items.clone()));

            b.switch_to(Storage::Vault, Some(MAIN), &ids).unwrap();
            assert_eq!(b.active(), Storage::Vault);
            assert!(
                system.entries(&ids).unwrap().is_empty(),
                "moved, not copied"
            );
            assert_eq!(plain(b.entries(&ids).unwrap()), plain(items.clone()));

            // The file holds them: a fresh vault opened with the main password sees them all.
            let fresh = VaultStore::new();
            fresh.unlock(b.vault_path(), MAIN).unwrap();
            assert_eq!(plain(fresh.entries(&ids).unwrap()), plain(items));
            // Switching to the storage in use changes nothing.
            b.switch_to(Storage::Vault, None, &ids).unwrap();
        }

        #[test]
        fn switch_back_to_keychain_moves_everything() {
            let dir = tempfile::tempdir().unwrap();
            let system = dpapi_store(dir.path());
            let (ids, items) = sample();
            let b = backend(SecretStorage::Vault, system.clone(), dir.path());
            b.create_vault(MAIN).unwrap();
            b.set_many(&items).unwrap();

            // A new session: the vault is locked until the main password is given.
            let b = backend(SecretStorage::Vault, system.clone(), dir.path());
            assert_eq!(b.needs_main_password(), Some(MainPassword::Unlock));
            assert!(matches!(
                b.get(&ids[0], SecretKind::Password),
                Err(SecretError::Locked)
            ));
            assert!(matches!(
                b.switch_to(Storage::System, None, &ids),
                Err(SecretError::Locked)
            ));
            assert!(matches!(
                b.switch_to(Storage::System, Some("not it"), &ids),
                Err(SecretError::WrongMainPassword)
            ));
            assert_eq!(b.active(), Storage::Vault);

            b.switch_to(Storage::System, Some(MAIN), &ids).unwrap();
            assert_eq!(b.active(), Storage::System);
            assert_eq!(plain(system.entries(&ids).unwrap()), plain(items.clone()));
            assert_eq!(plain(b.entries(&ids).unwrap()), plain(items));
            // The vault no longer holds them.
            let fresh = VaultStore::new();
            fresh.unlock(b.vault_path(), MAIN).unwrap();
            assert!(fresh.entries(&ids).unwrap().is_empty());
        }

        #[test]
        fn switch_aborts_when_old_store_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            let flaky = Arc::new(Flaky::default());
            let (ids, items) = sample();
            flaky.set_many(&items).unwrap();
            let b = backend(SecretStorage::Keychain, flaky.clone(), dir.path());

            // The keychain errors on read: nothing moved, nothing deleted, no vault made.
            flaky.fail_reads.store(true, Ordering::SeqCst);
            let err = b.switch_to(Storage::Vault, Some(MAIN), &ids).unwrap_err();
            assert!(matches!(err, SecretError::Backend(_)), "{err:?}");
            assert_eq!(b.active(), Storage::System);
            assert_eq!(flaky.removals.load(Ordering::SeqCst), 0);
            assert!(!b.vault_exists());
            flaky.fail_reads.store(false, Ordering::SeqCst);
            assert_eq!(plain(flaky.entries(&ids).unwrap()), plain(items.clone()));

            // Into the vault and back, with the keychain refusing the write: the vault keeps
            // everything and stays in use.
            b.switch_to(Storage::Vault, Some(MAIN), &ids).unwrap();
            assert!(flaky.entries(&ids).unwrap().is_empty());
            let removals = flaky.removals.load(Ordering::SeqCst);
            flaky.fail_writes.store(true, Ordering::SeqCst);
            assert!(b.switch_to(Storage::System, None, &ids).is_err());
            assert_eq!(b.active(), Storage::Vault);
            assert_eq!(plain(b.entries(&ids).unwrap()), plain(items.clone()));

            // A write that reads back different: cancelled, nothing removed from the vault.
            flaky.fail_writes.store(false, Ordering::SeqCst);
            flaky.lose_writes.store(true, Ordering::SeqCst);
            let err = b.switch_to(Storage::System, None, &ids).unwrap_err();
            assert!(matches!(err, SecretError::NotVerified), "{err:?}");
            assert_eq!(b.active(), Storage::Vault);
            assert_eq!(plain(b.entries(&ids).unwrap()), plain(items));
            assert_eq!(flaky.removals.load(Ordering::SeqCst), removals);
        }

        #[test]
        fn reset_vault_forgets_secrets_and_asks_for_a_new_main_password() {
            let dir = tempfile::tempdir().unwrap();
            let b = backend(
                SecretStorage::Vault,
                Arc::new(MemoryStore::new()),
                dir.path(),
            );
            b.create_vault(MAIN).unwrap();
            let id = pid();
            b.set(&id, SecretKind::Password, "pw").unwrap();
            b.reset_vault().unwrap();
            assert!(!b.vault_exists());
            assert!(!b.vault_unlocked());
            assert_eq!(b.active(), Storage::Vault, "still the vault");
            assert_eq!(b.needs_main_password(), Some(MainPassword::Set));
            assert_eq!(b.get(&id, SecretKind::Password).unwrap(), None);
            // A locked vault resets too (a forgotten main password).
            b.create_vault("another").unwrap();
            let b = backend(
                SecretStorage::Vault,
                Arc::new(MemoryStore::new()),
                dir.path(),
            );
            assert_eq!(b.needs_main_password(), Some(MainPassword::Unlock));
            b.reset_vault().unwrap();
            assert!(!b.vault_exists());
        }

        #[test]
        fn unlock_opens_the_vault_in_use() {
            let dir = tempfile::tempdir().unwrap();
            let b = backend(
                SecretStorage::Vault,
                Arc::new(MemoryStore::new()),
                dir.path(),
            );
            b.create_vault(MAIN).unwrap();
            let id = pid();
            b.set(&id, SecretKind::Password, "pw").unwrap();
            let b = backend(
                SecretStorage::Vault,
                Arc::new(MemoryStore::new()),
                dir.path(),
            );
            assert!(matches!(
                b.unlock_vault("wrong"),
                Err(SecretError::WrongMainPassword)
            ));
            assert!(!b.vault_unlocked());
            b.unlock_vault(MAIN).unwrap();
            assert_eq!(
                b.get(&id, SecretKind::Password).unwrap().as_deref(),
                Some("pw")
            );
            // Already open: any answer does (the app may answer a waiting connect late).
            b.unlock_vault("").unwrap();
        }

        #[test]
        fn memory_backend_switches_in_a_temporary_folder() {
            let b = SecretBackend::in_memory(SecretStorage::Auto);
            let folder = b.vault_path().parent().unwrap().to_path_buf();
            assert!(folder.starts_with(std::env::temp_dir()));
            let (ids, items) = sample();
            b.set_many(&items).unwrap();
            b.switch_to(Storage::Vault, Some(MAIN), &ids).unwrap();
            assert!(b.vault_exists());
            drop(b);
            assert!(!folder.exists(), "removed with the backend");
        }

        #[test]
        fn backend_is_send_and_sync() {
            fn check<T: Send + Sync>() {}
            check::<SecretBackend>();
        }
    }

    /// The OS keychain store (not built on Windows, whose system storage is DPAPI).
    #[cfg(not(windows))]
    mod keychain {
        use super::*;
        use crate::keychain::BlobBackend;
        use parking_lot::Mutex;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

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

        #[test]
        fn keychain_remove_kind_rewrites_only_when_needed() {
            let mock = Arc::new(MockBlob::default());
            let s = KeychainStore::with_backend(Box::new(mock.clone()));
            let (a, b) = (pid(), pid());
            s.set(&a, SecretKind::Password, "pw").unwrap();
            s.set(&a, SecretKind::KeyPassphrase, "pp").unwrap();
            s.set(&b, SecretKind::Password, "other").unwrap();
            s.remove_kind(&a, SecretKind::Password).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&mock.blob.lock().clone().unwrap()).unwrap();
            assert!(v[a.to_string()].get("password").is_none());
            assert_eq!(v[a.to_string()]["key_passphrase"], "pp");
            assert_eq!(v[b.to_string()]["password"], "other");
            let stores = mock.stores.load(Ordering::SeqCst);
            s.remove_kind(&a, SecretKind::Password).unwrap();
            s.remove_kind(&pid(), SecretKind::Password).unwrap();
            assert_eq!(
                mock.stores.load(Ordering::SeqCst),
                stores,
                "nothing to clear"
            );
            s.remove_kind(&a, SecretKind::KeyPassphrase).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&mock.blob.lock().clone().unwrap()).unwrap();
            assert!(v.get(a.to_string()).is_none(), "empty entries are pruned");
        }

        #[test]
        fn backend_remove_kind_reaches_the_keychain() {
            let mock = Arc::new(MockBlob::default());
            let dir = tempfile::tempdir().unwrap();
            let s = SecretBackend::with_system(
                SecretStorage::Keychain,
                Box::new(KeychainStore::with_backend(Box::new(mock.clone()))),
                dir.path().join("secrets.fbvault"),
            );
            let id = pid();
            s.set(&id, SecretKind::Password, "pw").unwrap();
            s.remove_kind(&id, SecretKind::Password).unwrap();
            assert_eq!(s.get(&id, SecretKind::Password).unwrap(), None);
            assert!(!mock.blob.lock().clone().unwrap().contains("pw"));
        }

        #[test]
        fn keychain_reports_entries() {
            let mock = Arc::new(MockBlob::default());
            let s = KeychainStore::with_backend(Box::new(mock.clone()));
            let (a, b, unknown) = (pid(), pid(), pid());
            let items = vec![
                entry(a, SecretKind::Password, "pw-a"),
                entry(a, SecretKind::KeyPassphrase, "pp-a"),
                entry(b, SecretKind::Password, "pw-\u{e9}"),
            ];
            s.set_many(&items).unwrap();
            assert_eq!(
                mock.stores.load(Ordering::SeqCst),
                1,
                "a batch is one keychain write"
            );
            let mut expected = plain(items.clone());
            assert_eq!(plain(s.entries(&[a, b, unknown]).unwrap()), expected);
            expected.retain(|(id, _, _)| *id == b);
            assert_eq!(plain(s.entries(&[b]).unwrap()), expected);
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
}
