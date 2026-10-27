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
/// [`SecretError::Unavailable`] (Auto may then move to the vault): on Linux a Secret Service
/// error that says no provider or no session is there (see [`no_service`]), or no keyring
/// store for this platform. Everything else (denied, locked or cancelled access, timeouts, a
/// prompt cut short, a keyring daemon slow to start) is [`SecretError::Backend`]: the storage
/// is there. macOS always has a keychain, and reports a user's "Deny" as a platform failure:
/// never unavailable there.
pub(crate) fn keychain_error(e: keyring::Error) -> SecretError {
    let setup = match &e {
        keyring::Error::NoDefaultStore => keyring::Entry::store_status().as_ref().err(),
        _ => None,
    };
    classify(e, setup)
}

/// [`keychain_error`] with the result of keyring's store setup at hand. keyring sets its
/// store up once per process and caches a failure: every later entry is `NoDefaultStore`,
/// and only the setup error (`setup`) says whether the service is missing or just failed
/// (for example a D-Bus timeout while the keyring daemon starts at login), so a
/// `NoDefaultStore` is classified by it.
fn classify(e: keyring::Error, setup: Option<&keyring::Error>) -> SecretError {
    let missing = match &e {
        keyring::Error::NoDefaultStore => match setup {
            Some(keyring::Error::PlatformFailure(source)) => no_service(&fault_of(source.as_ref())),
            // keyring's own "this platform has no store".
            Some(keyring::Error::Invalid(param, _)) => param == "platform",
            _ => false,
        },
        keyring::Error::PlatformFailure(source) => no_service(&fault_of(source.as_ref())),
        _ => false,
    };
    let text = match setup {
        Some(cause) => format!("{e}: {cause}"),
        None => e.to_string(),
    };
    if missing {
        SecretError::Unavailable(text)
    } else {
        SecretError::Backend(text)
    }
}

/// What a Secret Service failure says, reduced to what Auto needs; platform neutral so the
/// rule is tested everywhere.
#[derive(Debug)]
#[cfg_attr(
    all(not(target_os = "linux"), not(test)),
    expect(dead_code, reason = "only the Secret Service (Linux) reports these")
)]
pub(crate) enum ServiceFault {
    /// No D-Bus session, or no Secret Service provider on it.
    NoService,
    /// A D-Bus error, by its name.
    Dbus(String),
    Other,
}

/// The D-Bus errors of a name nobody owns: no keyring provider on the session bus.
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";
const NAME_HAS_NO_OWNER: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";

/// Whether `fault` means there is no Secret Service at all.
pub(crate) fn no_service(fault: &ServiceFault) -> bool {
    match fault {
        ServiceFault::NoService => true,
        ServiceFault::Dbus(name) => name == SERVICE_UNKNOWN || name == NAME_HAS_NO_OWNER,
        ServiceFault::Other => false,
    }
}

/// The fault in a boxed platform error: on Linux the Secret Service error keyring boxes.
#[cfg(target_os = "linux")]
fn fault_of(source: &(dyn std::error::Error + Send + Sync + 'static)) -> ServiceFault {
    use secret_service::Error as Service;
    fn fdo(e: &zbus::fdo::Error) -> ServiceFault {
        match e {
            zbus::fdo::Error::ServiceUnknown(_) => ServiceFault::Dbus(SERVICE_UNKNOWN.into()),
            zbus::fdo::Error::NameHasNoOwner(_) => ServiceFault::Dbus(NAME_HAS_NO_OWNER.into()),
            _ => ServiceFault::Other,
        }
    }
    fn bus(e: &zbus::Error) -> ServiceFault {
        match e {
            zbus::Error::MethodError(name, _, _) => ServiceFault::Dbus(name.as_str().to_owned()),
            zbus::Error::FDO(e) => fdo(e),
            _ => ServiceFault::Other,
        }
    }
    match source.downcast_ref::<Service>() {
        Some(Service::Unavailable) => ServiceFault::NoService,
        Some(Service::ZbusFdo(e)) => fdo(e),
        Some(Service::Zbus(e)) => bus(e),
        _ => ServiceFault::Other,
    }
}

/// Elsewhere (macOS, other Unixes) a platform failure never means "no storage".
#[cfg(not(target_os = "linux"))]
fn fault_of(_source: &(dyn std::error::Error + Send + Sync + 'static)) -> ServiceFault {
    ServiceFault::Other
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

    /// All of `changes` in one keychain write: all or nothing (none if nothing changes).
    fn apply_changes(&self, changes: &[crate::SecretChange]) -> Result<(), SecretError> {
        use crate::SecretChange;
        let mut guard = self.loaded()?;
        let mut map = guard.clone().unwrap_or_default();
        let mut changed = false;
        for change in changes {
            match change {
                SecretChange::Set((id, kind, value)) => {
                    map.entry(id.to_string()).or_default().set(*kind, value);
                    changed = true;
                }
                SecretChange::Remove(id) => changed |= map.remove(&id.to_string()).is_some(),
                SecretChange::RemoveKind(id, kind) => {
                    let key = id.to_string();
                    if let Some(secrets) = map.get_mut(&key)
                        && secrets.clear(*kind)
                    {
                        changed = true;
                        if secrets.is_empty() {
                            map.remove(&key);
                        }
                    }
                }
            }
        }
        if !changed {
            return Ok(());
        }
        self.commit(&mut guard, map)
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
        // A platform failure that is not a Secret Service error (and every one on macOS,
        // where a user's "Deny" arrives as one): the storage is there.
        let failure = keychain_error(platform("something failed"));
        assert!(matches!(failure, SecretError::Backend(_)), "{failure:?}");
    }

    #[test]
    fn a_failed_store_setup_is_unavailable_only_when_nothing_is_there() {
        // keyring caches its one store setup: every later entry is `NoDefaultStore`, and
        // the setup error says why. A setup failure that is not "no service" (here a plain
        // platform failure, on Linux a D-Bus timeout) keeps the storage.
        let flaky = platform("timed out");
        let e = classify(keyring::Error::NoDefaultStore, Some(&flaky));
        assert!(matches!(e, SecretError::Backend(_)), "{e:?}");
        // An unsupported platform has no storage at all.
        let unsupported = keyring::Error::Invalid("platform".into(), "unsupported".into());
        let e = classify(keyring::Error::NoDefaultStore, Some(&unsupported));
        assert!(matches!(e, SecretError::Unavailable(_)), "{e:?}");
        // No setup error to look at: the storage may be there.
        let e = classify(keyring::Error::NoDefaultStore, None);
        assert!(matches!(e, SecretError::Backend(_)), "{e:?}");
    }

    #[test]
    fn only_these_service_faults_mean_no_service() {
        assert!(no_service(&ServiceFault::NoService));
        for name in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert!(no_service(&ServiceFault::Dbus(name.into())), "{name}");
        }
        for name in [
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.Timeout",
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.Secret.Error.IsLocked",
        ] {
            assert!(!no_service(&ServiceFault::Dbus(name.into())), "{name}");
        }
        assert!(!no_service(&ServiceFault::Other));
    }

    #[test]
    fn one_write_for_a_batch_of_changes() {
        use crate::SecretChange;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Default)]
        struct Blob {
            blob: parking_lot::Mutex<Option<String>>,
            stores: AtomicUsize,
            fail: std::sync::atomic::AtomicBool,
        }
        impl BlobBackend for Arc<Blob> {
            fn load(&self) -> Result<Option<String>, SecretError> {
                Ok(self.blob.lock().clone())
            }
            fn store(&self, blob: &str) -> Result<(), SecretError> {
                if self.fail.load(Ordering::SeqCst) {
                    return Err(SecretError::Backend("denied".into()));
                }
                self.stores.fetch_add(1, Ordering::SeqCst);
                *self.blob.lock() = Some(blob.to_owned());
                Ok(())
            }
        }
        let blob = Arc::new(Blob::default());
        let s = KeychainStore::with_backend(Box::new(blob.clone()));
        let (a, b) = (ProfileId::new(), ProfileId::new());
        s.set(&a, SecretKind::KeyPassphrase, "old").unwrap();
        s.set(&b, SecretKind::Password, "gone").unwrap();
        let stores = blob.stores.load(Ordering::SeqCst);
        let set = |id, kind, v: &str| SecretChange::Set((id, kind, Zeroizing::new(v.into())));
        s.apply_changes(&[
            set(a, SecretKind::Password, "pw"),
            SecretChange::RemoveKind(a, SecretKind::KeyPassphrase),
            SecretChange::Remove(b),
        ])
        .unwrap();
        assert_eq!(blob.stores.load(Ordering::SeqCst), stores + 1, "one write");
        assert_eq!(
            s.get(&a, SecretKind::Password).unwrap().as_deref(),
            Some("pw")
        );
        assert_eq!(s.get(&a, SecretKind::KeyPassphrase).unwrap(), None);
        assert_eq!(s.get(&b, SecretKind::Password).unwrap(), None);
        // A refused write changes nothing at all.
        blob.fail.store(true, Ordering::SeqCst);
        assert!(
            s.apply_changes(&[set(b, SecretKind::Password, "x"), SecretChange::Remove(a)])
                .is_err()
        );
        assert_eq!(
            s.get(&a, SecretKind::Password).unwrap().as_deref(),
            Some("pw")
        );
        assert_eq!(s.get(&b, SecretKind::Password).unwrap(), None);
        // Nothing to change: nothing written.
        blob.fail.store(false, Ordering::SeqCst);
        s.apply_changes(&[SecretChange::Remove(ProfileId::new())])
            .unwrap();
        assert_eq!(blob.stores.load(Ordering::SeqCst), stores + 1);
    }

    /// The real Secret Service errors (Linux, where the keychain is the Secret Service).
    #[cfg(target_os = "linux")]
    #[test]
    fn secret_service_errors_are_classified() {
        let wrap = |e: secret_service::Error| keyring::Error::PlatformFailure(Box::new(e));
        let unavailable = |e| matches!(keychain_error(wrap(e)), SecretError::Unavailable(_));
        assert!(unavailable(secret_service::Error::Unavailable));
        assert!(unavailable(secret_service::Error::ZbusFdo(
            zbus::fdo::Error::ServiceUnknown("no keyring".into())
        )));
        assert!(unavailable(secret_service::Error::ZbusFdo(
            zbus::fdo::Error::NameHasNoOwner("no keyring".into())
        )));
        assert!(unavailable(secret_service::Error::Zbus(zbus::Error::FDO(
            Box::new(zbus::fdo::Error::ServiceUnknown("no keyring".into()))
        ))));
        // The same errors from keyring's cached store setup (`NoDefaultStore` on every
        // later entry): only a missing service moves Auto.
        let setup =
            |e: secret_service::Error| classify(keyring::Error::NoDefaultStore, Some(&wrap(e)));
        assert!(matches!(
            setup(secret_service::Error::Unavailable),
            SecretError::Unavailable(_)
        ));
        assert!(matches!(
            setup(secret_service::Error::ZbusFdo(
                zbus::fdo::Error::ServiceUnknown("no keyring".into())
            )),
            SecretError::Unavailable(_)
        ));
        for e in [
            secret_service::Error::ZbusFdo(zbus::fdo::Error::NoReply("slow".into())),
            secret_service::Error::ZbusFdo(zbus::fdo::Error::Timeout("slow".into())),
            secret_service::Error::Zbus(zbus::Error::InputOutput(std::sync::Arc::new(
                std::io::Error::other("hiccup"),
            ))),
        ] {
            let shown = e.to_string();
            let got = setup(e);
            assert!(matches!(got, SecretError::Backend(_)), "{shown}: {got:?}");
        }
        for e in [
            secret_service::Error::Locked,
            secret_service::Error::Prompt,
            secret_service::Error::PromptDisconnected,
            secret_service::Error::NoResult,
            secret_service::Error::Crypto("bad"),
            secret_service::Error::ZbusFdo(zbus::fdo::Error::NoReply("slow".into())),
            secret_service::Error::ZbusFdo(zbus::fdo::Error::Timeout("slow".into())),
            secret_service::Error::Zbus(zbus::Error::InputOutput(std::sync::Arc::new(
                std::io::Error::other("hiccup"),
            ))),
        ] {
            let shown = e.to_string();
            assert!(!unavailable(e), "{shown}");
        }
    }
}
