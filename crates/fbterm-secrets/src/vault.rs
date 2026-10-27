//! The encrypted secrets vault (`<config>/secrets.fbvault`): every saved secret in one
//! `age` file with a passphrase (scrypt) recipient, unlocked with the user's main password.
//!
//! The file holds a versioned TOML document (`format = 1`, then the secrets) and is only
//! ever written encrypted: every change re-encrypts the whole document into a private part
//! file next to the vault, which then replaces it. The contents and the main password live
//! in memory, in wiped buffers, only while the vault is unlocked; [`VaultStore::lock`]
//! drops them.
//!
//! Every write and every unlock runs scrypt at age's default work factor (about 0.6 s in a
//! release build), so callers keep them off the UI thread and save batches with
//! [`SecretStore::set_many`], which writes the file once.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use fbterm_config::ProfileId;
use parking_lot::Mutex;
use zeroize::Zeroizing;

use crate::archive::{self, ArchiveError};
use crate::secrets_file::{self, Contents, ParseError, ReadError};
use crate::{SecretEntry, SecretError, SecretKind, SecretStore};

/// The vault format this build writes and the newest it reads.
pub const VAULT_FORMAT: u32 = 1;

/// The largest vault file `unlock` reads. A real vault is a few kilobytes; the cap keeps a
/// huge or wrong file from exhausting memory.
pub const MAX_VAULT_BYTES: u64 = 16 * 1024 * 1024;

const MISSING: &str = "No encrypted vault was found.";
const TOO_LARGE: &str = "The file is too large to be an fbterm vault.";
const DAMAGED: &str = "The vault is damaged or incomplete.";
const UNREADABLE: &str = "The vault's contents could not be read.";
const NEWER: &str = "This vault was made by a newer fbterm.";
const TOO_HARD: &str = "This vault takes too much work to open on this computer.";
const NOT_READ: &str = "The vault could not be read.";
const NOT_WRITTEN: &str = "The vault could not be written.";
const NOT_DELETED: &str = "The vault could not be deleted.";

/// What exists only while the vault is unlocked.
struct Unlocked {
    path: PathBuf,
    /// Kept to re-encrypt on every write.
    password: Zeroizing<String>,
    contents: Contents,
}

/// Saved secrets in the encrypted vault. Starts locked: every [`SecretStore`] call returns
/// [`SecretError::Locked`] until [`unlock`](Self::unlock) or [`create`](Self::create)
/// succeeds.
#[derive(Default)]
pub struct VaultStore {
    state: Mutex<Option<Unlocked>>,
    /// Files written by this store (tests check that a batch is one write).
    writes: AtomicUsize,
}

impl VaultStore {
    /// A locked store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a vault file (or anything else) is at `path`.
    pub fn exists(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    /// Make a new, empty vault at `path` encrypted with `password` and leave it unlocked.
    /// Refuses with [`SecretError::VaultExists`] when something is already at `path`.
    pub fn create(&self, path: &Path, password: &str) -> Result<(), SecretError> {
        if Self::exists(path) {
            return Err(SecretError::VaultExists);
        }
        let unlocked = Unlocked {
            path: path.to_path_buf(),
            password: Zeroizing::new(password.to_owned()),
            contents: Contents::new(),
        };
        self.write(&unlocked.path, &unlocked.password, &unlocked.contents)?;
        *self.state.lock() = Some(unlocked);
        Ok(())
    }

    /// Open the vault at `path` with `password` and keep its contents in memory until
    /// [`lock`](Self::lock). A wrong password, or a file that is not a vault, is
    /// [`SecretError::WrongMainPassword`]; the store stays as it was on any error.
    pub fn unlock(&self, path: &Path, password: &str) -> Result<(), SecretError> {
        let bytes = read_bounded(path)?;
        let plain = archive::decrypt(&bytes, password).map_err(|e| match e {
            ArchiveError::WrongPassword => SecretError::WrongMainPassword,
            ArchiveError::Corrupt(m) if m == archive::TOO_HARD => vault_err(TOO_HARD),
            _ => vault_err(DAMAGED),
        })?;
        let contents = parse(&plain)?;
        *self.state.lock() = Some(Unlocked {
            path: path.to_path_buf(),
            password: Zeroizing::new(password.to_owned()),
            contents,
        });
        Ok(())
    }

    pub fn is_unlocked(&self) -> bool {
        self.state.lock().is_some()
    }

    /// Forget the contents and the main password (wiped as they drop). The file stays.
    pub fn lock(&self) {
        self.state.lock().take();
    }

    /// Delete the vault at `path` and lock: for a forgotten main password, so it works
    /// while locked. A missing file is not an error.
    pub fn reset(&self, path: &Path) -> Result<(), SecretError> {
        self.lock();
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(vault_err(NOT_DELETED)),
        }
    }

    #[cfg(test)]
    fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    /// Apply `change` to a copy of the contents and, if it changed anything, write the
    /// copy and adopt it. A failed write leaves the file and the memory as they were.
    fn update(&self, change: impl FnOnce(&mut Contents) -> bool) -> Result<(), SecretError> {
        let mut guard = self.state.lock();
        let unlocked = guard.as_mut().ok_or(SecretError::Locked)?;
        let mut next = unlocked.contents.clone();
        if !change(&mut next) {
            return Ok(());
        }
        self.write(&unlocked.path, &unlocked.password, &next)?;
        unlocked.contents = next;
        Ok(())
    }

    /// Encrypt `contents` and atomically replace the file at `path`.
    fn write(&self, path: &Path, password: &str, contents: &Contents) -> Result<(), SecretError> {
        let text =
            secrets_file::to_toml(contents, VAULT_FORMAT).ok_or_else(|| vault_err(NOT_WRITTEN))?;
        let sealed =
            archive::encrypt(text.as_bytes(), password).map_err(|_| vault_err(NOT_WRITTEN))?;
        secrets_file::write_atomic(path, &sealed).map_err(|e| {
            tracing::warn!(error = %e, "could not write the secrets vault");
            vault_err(NOT_WRITTEN)
        })?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl SecretStore for VaultStore {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        let guard = self.state.lock();
        let unlocked = guard.as_ref().ok_or(SecretError::Locked)?;
        Ok(unlocked
            .contents
            .get(&(*id, kind))
            .map(|v| v.as_str().to_owned()))
    }

    fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError> {
        self.update(|c| {
            c.insert((*id, kind), Zeroizing::new(value.to_owned()));
            true
        })
    }

    fn remove(&self, id: &ProfileId) -> Result<(), SecretError> {
        self.update(|c| {
            let mut changed = false;
            for kind in SecretKind::ALL {
                changed |= c.remove(&(*id, kind)).is_some();
            }
            changed
        })
    }

    fn remove_kind(&self, id: &ProfileId, kind: SecretKind) -> Result<(), SecretError> {
        self.update(|c| c.remove(&(*id, kind)).is_some())
    }

    fn entries(&self, ids: &[ProfileId]) -> Result<Vec<SecretEntry>, SecretError> {
        let guard = self.state.lock();
        let unlocked = guard.as_ref().ok_or(SecretError::Locked)?;
        Ok(unlocked
            .contents
            .iter()
            .filter(|((id, _), _)| ids.contains(id))
            .map(|((id, kind), value)| (*id, *kind, value.clone()))
            .collect())
    }

    /// All of `items` in one re-encryption and one file write.
    fn set_many(&self, items: &[SecretEntry]) -> Result<(), SecretError> {
        self.update(|c| {
            for (id, kind, value) in items {
                c.insert((*id, *kind), value.clone());
            }
            !items.is_empty()
        })
    }
}

fn vault_err(message: &str) -> SecretError {
    SecretError::Vault(message.to_owned())
}

/// Read the vault file, refusing anything larger than [`MAX_VAULT_BYTES`].
fn read_bounded(path: &Path) -> Result<Vec<u8>, SecretError> {
    secrets_file::read_bounded(path, MAX_VAULT_BYTES).map_err(|e| {
        vault_err(match e {
            ReadError::Missing => MISSING,
            ReadError::TooLarge => TOO_LARGE,
            ReadError::Io => NOT_READ,
        })
    })
}

/// Parse the decrypted document; the errors quote nothing from it.
fn parse(plain: &[u8]) -> Result<Contents, SecretError> {
    secrets_file::parse(plain, VAULT_FORMAT).map_err(|e| {
        vault_err(match e {
            ParseError::Newer => NEWER,
            ParseError::Unreadable => UNREADABLE,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use zeroize::Zeroizing;

    const MAIN: &str = "main password for tests";
    const SECRET: &str = "vault-secret-value-7d1f";

    /// Every vault write or unlock costs a full scrypt run at age's default work factor, so
    /// the sample vault is made once and copied into each test's own directory.
    fn fixture() -> &'static (ProfileId, Vec<u8>) {
        static FIXTURE: OnceLock<(ProfileId, Vec<u8>)> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secrets.fbvault");
            let id = ProfileId::new();
            let store = VaultStore::new();
            store.create(&path, MAIN).unwrap();
            // create leaves the vault unlocked and empty; one more write adds the secret.
            store.set(&id, SecretKind::Password, SECRET).unwrap();
            (id, std::fs::read(&path).unwrap())
        })
    }

    /// A temp dir holding a copy of the fixture vault at the standard path.
    fn fixture_dir() -> (tempfile::TempDir, PathBuf, ProfileId) {
        let (id, bytes) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = fbterm_config::Paths {
            config_dir: dir.path().to_path_buf(),
        }
        .vault();
        std::fs::write(&path, bytes).unwrap();
        (dir, path, *id)
    }

    fn entry(id: ProfileId, kind: SecretKind, value: &str) -> SecretEntry {
        (id, kind, Zeroizing::new(value.to_owned()))
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn vault_path_is_in_the_config_dir() {
        let paths = fbterm_config::Paths {
            config_dir: PathBuf::from("cfg"),
        };
        assert_eq!(paths.vault(), Path::new("cfg").join("secrets.fbvault"));
    }

    #[test]
    fn vault_round_trip() {
        let (dir, path, id) = fixture_dir();
        let store = VaultStore::new();
        store.unlock(&path, MAIN).unwrap();
        assert!(store.is_unlocked());
        assert_eq!(
            store.get(&id, SecretKind::Password).unwrap().as_deref(),
            Some(SECRET)
        );
        let other = ProfileId::new();
        store
            .set_many(&[
                entry(id, SecretKind::KeyPassphrase, "pp"),
                entry(other, SecretKind::Password, "other-pw"),
            ])
            .unwrap();

        // A fresh store sees what was written.
        let again = VaultStore::new();
        again.unlock(&path, MAIN).unwrap();
        let mut all: Vec<_> = again
            .entries(&[id, other, ProfileId::new()])
            .unwrap()
            .into_iter()
            .map(|(i, k, v)| (i, k, v.as_str().to_owned()))
            .collect();
        all.sort();
        let mut expected = vec![
            (id, SecretKind::Password, SECRET.to_owned()),
            (id, SecretKind::KeyPassphrase, "pp".to_owned()),
            (other, SecretKind::Password, "other-pw".to_owned()),
        ];
        expected.sort();
        assert_eq!(all, expected);
        assert_eq!(again.entries(&[other]).unwrap().len(), 1, "filtered by id");

        // Removing a kind keeps the other; removing nothing writes nothing.
        again.remove_kind(&id, SecretKind::Password).unwrap();
        assert_eq!(again.get(&id, SecretKind::Password).unwrap(), None);
        assert_eq!(
            again
                .get(&id, SecretKind::KeyPassphrase)
                .unwrap()
                .as_deref(),
            Some("pp")
        );
        let writes = again.writes();
        again.remove_kind(&id, SecretKind::Password).unwrap();
        again.remove(&ProfileId::new()).unwrap();
        assert_eq!(again.writes(), writes);
        again.remove(&other).unwrap();
        assert_eq!(again.get(&other, SecretKind::Password).unwrap(), None);

        // Only the vault itself is left behind, private to the user.
        assert_eq!(dir_names(dir.path()), vec!["secrets.fbvault".to_owned()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn locked_vault_reports_locked() {
        let store = VaultStore::new();
        assert!(!store.is_unlocked());
        let id = ProfileId::new();
        assert!(matches!(
            store.get(&id, SecretKind::Password),
            Err(SecretError::Locked)
        ));
        assert!(matches!(
            store.set(&id, SecretKind::Password, "x"),
            Err(SecretError::Locked)
        ));
        assert!(matches!(
            store.set_many(&[entry(id, SecretKind::Password, "x")]),
            Err(SecretError::Locked)
        ));
        assert!(matches!(store.remove(&id), Err(SecretError::Locked)));
        assert!(matches!(
            store.remove_kind(&id, SecretKind::Password),
            Err(SecretError::Locked)
        ));
        assert!(matches!(store.entries(&[id]), Err(SecretError::Locked)));
    }

    #[test]
    fn lock_wipes_contents() {
        let (_dir, path, id) = fixture_dir();
        let store = VaultStore::new();
        store.unlock(&path, MAIN).unwrap();
        store.lock();
        assert!(!store.is_unlocked());
        assert!(store.state.lock().is_none(), "nothing kept in memory");
        assert!(matches!(
            store.get(&id, SecretKind::Password),
            Err(SecretError::Locked)
        ));
        assert!(path.exists(), "locking keeps the file");
    }

    #[test]
    fn wrong_main_password_is_refused() {
        let (_dir, path, _) = fixture_dir();
        let store = VaultStore::new();
        let err = store.unlock(&path, "not the main password").unwrap_err();
        assert!(matches!(err, SecretError::WrongMainPassword));
        assert_eq!(err.to_string(), "Wrong main password.");
        assert!(!store.is_unlocked());

        // Not an age file at all reads the same way: nothing about the contents leaks.
        std::fs::write(&path, b"plain text, not a vault").unwrap();
        assert!(matches!(
            store.unlock(&path, MAIN),
            Err(SecretError::WrongMainPassword)
        ));
    }

    #[test]
    fn missing_or_oversized_vault_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.fbvault");
        let store = VaultStore::new();
        assert!(!VaultStore::exists(&path));
        assert!(matches!(
            store.unlock(&path, MAIN),
            Err(SecretError::Vault(_))
        ));
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_VAULT_BYTES + 1).unwrap();
        assert!(VaultStore::exists(&path));
        let err = store.unlock(&path, MAIN).unwrap_err();
        assert!(matches!(err, SecretError::Vault(_)), "{err:?}");
    }

    #[test]
    fn vault_file_never_contains_plaintext() {
        let (id, bytes) = fixture();
        assert!(bytes.starts_with(b"age-encryption.org/v1\n"));
        let hay = String::from_utf8_lossy(bytes);
        for needle in [SECRET, &id.to_string(), "password", "format"] {
            assert!(!hay.contains(needle), "{needle} must not appear");
        }
        // The decrypted payload is the versioned TOML.
        let plain = crate::archive::decrypt(bytes, MAIN).unwrap();
        let text = std::str::from_utf8(&plain).unwrap();
        assert!(text.contains("format = 1"), "{}", text.len());
    }

    #[test]
    fn contents_are_versioned_and_errors_quote_nothing() {
        let id = ProfileId::new();
        let ok = format!(
            "format = 1\n[[secrets]]\nprofile = \"{id}\"\nkind = \"password\"\nvalue = \"{SECRET}\"\n"
        );
        let contents = parse(ok.as_bytes()).unwrap();
        assert_eq!(
            contents
                .get(&(id, SecretKind::Password))
                .map(|v| v.as_str()),
            Some(SECRET)
        );
        assert!(parse(b"format = 1\n").unwrap().is_empty());
        let newer = parse(b"format = 2\n").unwrap_err();
        assert_eq!(newer.to_string(), "This vault was made by a newer fbterm.");
        let bad = format!("format = 1\n[[secrets]]\nvalue = \"{SECRET}\"\n");
        for text in [bad.as_str(), "secrets = []\n", "not toml {"] {
            let err = parse(text.as_bytes()).unwrap_err();
            assert!(matches!(err, SecretError::Vault(_)), "{err:?}");
            assert!(!err.to_string().contains(SECRET));
        }
    }

    #[test]
    fn create_refuses_to_overwrite() {
        let (_dir, path, _) = fixture_dir();
        let before = std::fs::read(&path).unwrap();
        let store = VaultStore::new();
        assert!(matches!(
            store.create(&path, "another password"),
            Err(SecretError::VaultExists)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!store.is_unlocked());
    }

    #[test]
    fn set_many_writes_the_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("secrets.fbvault");
        let store = VaultStore::new();
        store.create(&path, MAIN).unwrap();
        assert!(store.is_unlocked());
        assert_eq!(store.writes(), 1);
        let items: Vec<_> = (0..5)
            .map(|i| entry(ProfileId::new(), SecretKind::Password, &format!("pw{i}")))
            .collect();
        store.set_many(&items).unwrap();
        assert_eq!(store.writes(), 2, "one re-encryption for the whole batch");
        let ids: Vec<_> = items.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(store.entries(&ids).unwrap().len(), 5);
    }

    #[test]
    fn reset_removes_file() {
        let (dir, path, id) = fixture_dir();
        // A forgotten main password: reset works while locked.
        let store = VaultStore::new();
        store.reset(&path).unwrap();
        assert!(!path.exists());
        assert!(dir_names(dir.path()).is_empty());
        // Resetting again (no file) is fine.
        store.reset(&path).unwrap();

        // Resetting an unlocked vault wipes memory too.
        std::fs::write(&path, &fixture().1).unwrap();
        store.unlock(&path, MAIN).unwrap();
        store.reset(&path).unwrap();
        assert!(!path.exists());
        assert!(!store.is_unlocked());
        assert!(matches!(
            store.get(&id, SecretKind::Password),
            Err(SecretError::Locked)
        ));
    }

    #[test]
    fn vault_store_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<VaultStore>();
    }
}
