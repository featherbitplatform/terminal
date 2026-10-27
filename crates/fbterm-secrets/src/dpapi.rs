//! The Windows system storage: every saved secret in one file, `<config>/secrets.dpapi`,
//! protected with DPAPI for the user's Windows account (no main password, no size limit).
//!
//! The file holds the same versioned TOML document as the vault (`format = 1`, then the
//! secrets), only ever written protected: every change rewrites the whole file through a
//! part file that then replaces it. Each call reads the file again, so nothing decrypted is
//! kept in memory between calls. A file that does not decrypt (made on another Windows
//! account, or damaged) is reported and never written over.
//!
//! The store is generic over a [`Codec`] so its logic is tested on every platform with a
//! stand-in; the real codec ([`Dpapi`]) exists only on Windows.

use std::path::PathBuf;

use fbterm_config::ProfileId;
use parking_lot::Mutex;
use zeroize::Zeroizing;

use crate::secrets_file::{self, Contents, ParseError, ReadError};
use crate::{SecretEntry, SecretError, SecretKind, SecretStore};

#[cfg(windows)]
pub use sys::Dpapi;

/// The file format this build writes and the newest it reads.
pub const DPAPI_FORMAT: u32 = 1;

/// The largest file read. A real one is a few kilobytes; the cap keeps a huge or wrong file
/// from exhausting memory.
pub const MAX_DPAPI_BYTES: u64 = 16 * 1024 * 1024;

const NOT_READ: &str = "Saved secrets could not be read.";
const TOO_LARGE: &str = "Saved secrets are too large to read.";
const OTHER_ACCOUNT: &str = "Saved secrets could not be read on this Windows account.";
const DAMAGED: &str = "Saved secrets are damaged.";
const NEWER: &str = "Saved secrets were made by a newer fbterm.";
const NOT_WRITTEN: &str = "Saved secrets could not be written.";

/// Turns the document into the bytes on disk and back.
pub trait Codec: Send + Sync {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, CodecFailed>;
    fn unprotect(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, CodecFailed>;
}

/// A codec call failed; the reason is not shown to the user.
#[derive(Debug)]
pub struct CodecFailed;

/// Saved secrets in one file protected by a [`Codec`].
pub struct CodecStore<C> {
    path: PathBuf,
    codec: C,
    /// Serializes read-modify-write cycles within this process.
    lock: Mutex<()>,
}

/// The Windows system storage: [`CodecStore`] over DPAPI.
#[cfg(windows)]
pub type DpapiStore = CodecStore<Dpapi>;

#[cfg(windows)]
impl CodecStore<Dpapi> {
    /// The store over the file at `path` (normally `Paths::dpapi`). Nothing is read until
    /// the first call.
    pub fn new(path: PathBuf) -> Self {
        Self::with_codec(path, Dpapi)
    }
}

impl<C: Codec> CodecStore<C> {
    pub(crate) fn with_codec(path: PathBuf, codec: C) -> Self {
        Self {
            path,
            codec,
            lock: Mutex::new(()),
        }
    }

    /// Read and decrypt the file; no file means nothing saved yet.
    fn load(&self) -> Result<Contents, SecretError> {
        let sealed = match secrets_file::read_bounded(&self.path, MAX_DPAPI_BYTES) {
            Ok(bytes) => bytes,
            Err(ReadError::Missing) => return Ok(Contents::new()),
            Err(ReadError::TooLarge) => return Err(storage_err(TOO_LARGE)),
            Err(ReadError::Io) => return Err(storage_err(NOT_READ)),
        };
        let plain = self
            .codec
            .unprotect(&sealed)
            .map_err(|_| storage_err(OTHER_ACCOUNT))?;
        secrets_file::parse(&plain, DPAPI_FORMAT).map_err(|e| {
            storage_err(match e {
                ParseError::Newer => NEWER,
                ParseError::Unreadable => DAMAGED,
            })
        })
    }

    /// The current contents, read under the lock.
    fn read(&self) -> Result<Contents, SecretError> {
        let _guard = self.lock.lock();
        self.load()
    }

    /// Apply `change` to the current contents and, if it changed anything, write them back.
    /// Nothing is written when the file cannot be read, and a failed write leaves the file
    /// as it was.
    fn update(&self, change: impl FnOnce(&mut Contents) -> bool) -> Result<(), SecretError> {
        let _guard = self.lock.lock();
        let mut contents = self.load()?;
        if !change(&mut contents) {
            return Ok(());
        }
        let text = secrets_file::to_toml(&contents, DPAPI_FORMAT)
            .ok_or_else(|| storage_err(NOT_WRITTEN))?;
        let sealed = self
            .codec
            .protect(text.as_bytes())
            .map_err(|_| storage_err(NOT_WRITTEN))?;
        secrets_file::write_atomic(&self.path, &sealed).map_err(|e| {
            tracing::warn!(error = %e, "could not write the saved secrets file");
            storage_err(NOT_WRITTEN)
        })
    }
}

impl<C: Codec> SecretStore for CodecStore<C> {
    fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError> {
        Ok(self
            .read()?
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
        Ok(self
            .read()?
            .into_iter()
            .filter(|((id, _), _)| ids.contains(id))
            .map(|((id, kind), value)| (id, kind, value))
            .collect())
    }

    /// All of `items` in one file write.
    fn set_many(&self, items: &[SecretEntry]) -> Result<(), SecretError> {
        if items.is_empty() {
            return Ok(());
        }
        self.update(|c| {
            for (id, kind, value) in items {
                c.insert((*id, *kind), value.clone());
            }
            true
        })
    }
}

fn storage_err(message: &str) -> SecretError {
    SecretError::Storage(message.to_owned())
}

/// The DPAPI calls, the only `unsafe` code in this crate.
#[cfg(windows)]
mod sys {
    use std::ptr;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use zeroize::{Zeroize, Zeroizing};

    use super::{Codec, CodecFailed};

    /// Mixed into every protection, so fbterm's file only decrypts together with this value
    /// and data protected by other programs is not taken for it. It ships in the binary, so
    /// it is a label rather than a secret.
    const ENTROPY: &[u8] = b"fbterm secrets v1";

    /// DPAPI for the current Windows user, never showing any UI.
    pub struct Dpapi;

    /// Log why the DPAPI call `call` failed (its `GetLastError` code, which says nothing about
    /// the data) and report the failure.
    fn failed(call: &'static str) -> CodecFailed {
        let code = std::io::Error::last_os_error().raw_os_error();
        tracing::warn!(call, code, "DPAPI call failed");
        CodecFailed
    }

    /// A borrowed input blob; DPAPI only reads it.
    fn input(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, CodecFailed> {
        Ok(CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(bytes.len()).map_err(|_| CodecFailed)?,
            pbData: bytes.as_ptr().cast_mut(),
        })
    }

    /// An output blob that DPAPI allocates; wiped and freed with `LocalFree` on drop.
    struct Output(CRYPT_INTEGER_BLOB);

    impl Output {
        fn new() -> Self {
            Self(CRYPT_INTEGER_BLOB {
                cbData: 0,
                pbData: ptr::null_mut(),
            })
        }

        fn bytes(&mut self) -> &mut [u8] {
            if self.0.pbData.is_null() {
                return &mut [];
            }
            // SAFETY: a successful DPAPI call set `pbData` to its own allocation of `cbData`
            // bytes, which this guard owns exclusively until it frees it in `drop`.
            unsafe { std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize) }
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            if self.0.pbData.is_null() {
                return;
            }
            self.bytes().zeroize();
            // SAFETY: `pbData` was allocated by DPAPI with `LocalAlloc` and is freed exactly
            // once, here; nothing reads it afterwards.
            unsafe { LocalFree(self.0.pbData.cast()) };
        }
    }

    impl Codec for Dpapi {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, CodecFailed> {
            let (data, entropy) = (input(plain)?, input(ENTROPY)?);
            let mut out = Output::new();
            // SAFETY: `data` and `entropy` describe live buffers of the stated lengths that
            // DPAPI only reads during the call; the description, reserved and prompt
            // arguments may be null; `out.0` is a valid place for DPAPI to store its
            // allocation, which `out` frees.
            let ok = unsafe {
                CryptProtectData(
                    &data,
                    ptr::null(),
                    &entropy,
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out.0,
                )
            };
            if ok == 0 {
                return Err(failed("CryptProtectData"));
            }
            Ok(out.bytes().to_vec())
        }

        fn unprotect(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, CodecFailed> {
            let (data, entropy) = (input(sealed)?, input(ENTROPY)?);
            let mut out = Output::new();
            // SAFETY: as in `protect`; the description out-pointer may be null, so DPAPI
            // returns no description to free.
            let ok = unsafe {
                CryptUnprotectData(
                    &data,
                    ptr::null_mut(),
                    &entropy,
                    ptr::null(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out.0,
                )
            };
            if ok == 0 {
                return Err(failed("CryptUnprotectData"));
            }
            // The plaintext goes straight into a wiped buffer; `out` wipes DPAPI's copy.
            Ok(Zeroizing::new(out.bytes().to_vec()))
        }
    }
}

/// A stand-in for DPAPI on every platform, for this crate's tests.
#[cfg(test)]
pub(crate) mod test_codec {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use zeroize::Zeroizing;

    use super::{Codec, CodecFailed};

    const MARK: &[u8] = b"FAKE";

    /// Stand-in for DPAPI on every platform: a marker, then the bytes inverted. Counts
    /// writes, and refuses to protect while `fail` is set.
    #[derive(Clone, Default)]
    pub(crate) struct Fake {
        pub(crate) writes: Arc<AtomicUsize>,
        pub(crate) fail: Arc<AtomicBool>,
    }

    impl Codec for Fake {
        fn protect(&self, plain: &[u8]) -> Result<Vec<u8>, CodecFailed> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(CodecFailed);
            }
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(MARK
                .iter()
                .copied()
                .chain(plain.iter().map(|b| !b))
                .collect())
        }
        fn unprotect(&self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, CodecFailed> {
            let rest = sealed.strip_prefix(MARK).ok_or(CodecFailed)?;
            Ok(Zeroizing::new(rest.iter().map(|b| !b).collect()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_codec::Fake;
    use super::*;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use zeroize::Zeroizing;

    const SECRET: &str = "dpapi-secret-value-3a9c";

    fn store(dir: &Path) -> (CodecStore<Fake>, Fake, PathBuf) {
        let path = fbterm_config::Paths {
            config_dir: dir.to_path_buf(),
        }
        .dpapi();
        let fake = Fake::default();
        (
            CodecStore::with_codec(path.clone(), fake.clone()),
            fake,
            path,
        )
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

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn dpapi_path_is_in_the_config_dir() {
        let paths = fbterm_config::Paths {
            config_dir: PathBuf::from("cfg"),
        };
        assert_eq!(paths.dpapi(), Path::new("cfg").join("secrets.dpapi"));
    }

    #[test]
    fn dpapi_map_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (s, fake, path) = store(dir.path());
        let (a, b) = (ProfileId::new(), ProfileId::new());

        // No file yet: nothing saved, and reading writes nothing.
        assert_eq!(s.get(&a, SecretKind::Password).unwrap(), None);
        assert!(s.entries(&[a, b]).unwrap().is_empty());
        assert!(!path.exists());

        s.set(&a, SecretKind::Password, SECRET).unwrap();
        let items = vec![
            entry(a, SecretKind::KeyPassphrase, "pp-a"),
            entry(b, SecretKind::Password, "pw-\u{e9}"),
        ];
        let writes = fake.writes.load(Ordering::SeqCst);
        s.set_many(&items).unwrap();
        assert_eq!(
            fake.writes.load(Ordering::SeqCst),
            writes + 1,
            "a batch is one write"
        );

        // A fresh store over the same file sees everything.
        let (again, _, _) = store(dir.path());
        assert_eq!(
            again.get(&a, SecretKind::Password).unwrap().as_deref(),
            Some(SECRET)
        );
        let mut expected = plain(items.clone());
        expected.push((a, SecretKind::Password, SECRET.to_owned()));
        expected.sort();
        assert_eq!(
            plain(again.entries(&[a, b, ProfileId::new()]).unwrap()),
            expected
        );
        assert_eq!(again.entries(&[b]).unwrap().len(), 1, "filtered by id");

        // The decoded file is the versioned document.
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8(fake.unprotect(&bytes).unwrap().to_vec()).unwrap();
        assert!(text.starts_with("format = 1\n"), "{}", text.len());

        // Removing a kind keeps the other; removing nothing writes nothing.
        again.remove_kind(&a, SecretKind::Password).unwrap();
        assert_eq!(again.get(&a, SecretKind::Password).unwrap(), None);
        assert_eq!(
            again.get(&a, SecretKind::KeyPassphrase).unwrap().as_deref(),
            Some("pp-a")
        );
        let writes = fake.writes.load(Ordering::SeqCst);
        let (third, third_fake, _) = store(dir.path());
        third.remove_kind(&a, SecretKind::Password).unwrap();
        third.remove(&ProfileId::new()).unwrap();
        third.set_many(&[]).unwrap();
        assert_eq!(third_fake.writes.load(Ordering::SeqCst), 0);
        assert_eq!(fake.writes.load(Ordering::SeqCst), writes);
        third.remove(&a).unwrap();
        assert!(third.entries(&[a]).unwrap().is_empty());
        assert_eq!(
            third.get(&b, SecretKind::Password).unwrap().as_deref(),
            Some("pw-\u{e9}")
        );
    }

    #[test]
    fn dpapi_write_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let (s, fake, path) = store(dir.path());
        let id = ProfileId::new();
        s.set(&id, SecretKind::Password, SECRET).unwrap();
        s.set(&id, SecretKind::KeyPassphrase, "pp").unwrap();
        // Only the file itself is left behind, private to the user.
        assert_eq!(dir_names(dir.path()), vec!["secrets.dpapi".to_owned()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // A failed write leaves the file as it was, and no part file.
        let before = std::fs::read(&path).unwrap();
        fake.fail.store(true, Ordering::SeqCst);
        let err = s.set(&id, SecretKind::Password, "new").unwrap_err();
        assert_eq!(err.to_string(), "Saved secrets could not be written.");
        assert!(matches!(
            s.set_many(&[entry(ProfileId::new(), SecretKind::Password, "x")]),
            Err(SecretError::Storage(_))
        ));
        assert!(matches!(s.remove(&id), Err(SecretError::Storage(_))));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(dir_names(dir.path()), vec!["secrets.dpapi".to_owned()]);
        fake.fail.store(false, Ordering::SeqCst);
        assert_eq!(
            s.get(&id, SecretKind::Password).unwrap().as_deref(),
            Some(SECRET)
        );

        // A missing config directory is created on the first write.
        let nested = dir.path().join("nested");
        let (n, _, npath) = store(&nested);
        n.set(&id, SecretKind::Password, SECRET).unwrap();
        assert!(npath.exists());
    }

    #[test]
    fn dpapi_corrupt_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (s, fake, path) = store(dir.path());
        let id = ProfileId::new();
        let account = "Saved secrets could not be read on this Windows account.";

        // Bytes that do not decrypt: another account's file, or not a DPAPI file at all.
        std::fs::write(&path, b"not protected").unwrap();
        let err = s.get(&id, SecretKind::Password).unwrap_err();
        assert!(matches!(err, SecretError::Storage(_)));
        assert_eq!(err.to_string(), account);
        // Nothing is written over a file that cannot be read: the secrets in it are kept.
        assert!(s.set(&id, SecretKind::Password, "x").is_err());
        assert!(s.set_many(&[entry(id, SecretKind::Password, "x")]).is_err());
        assert!(s.remove(&id).is_err());
        assert!(s.remove_kind(&id, SecretKind::Password).is_err());
        assert!(s.entries(&[id]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not protected");
        assert_eq!(fake.writes.load(Ordering::SeqCst), 0);

        // Decrypts, but is not the document; errors quote nothing from it.
        let bad = format!("format = 1\n[[secrets]]\nvalue = \"{SECRET}\"\n");
        for text in [bad.as_str(), "secrets = []\n", "not toml {"] {
            std::fs::write(&path, fake.protect(text.as_bytes()).unwrap()).unwrap();
            let err = s.get(&id, SecretKind::Password).unwrap_err();
            assert_eq!(err.to_string(), "Saved secrets are damaged.");
            assert!(!err.to_string().contains(SECRET));
        }
        std::fs::write(&path, fake.protect(b"format = 2\n").unwrap()).unwrap();
        let err = s.get(&id, SecretKind::Password).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Saved secrets were made by a newer fbterm."
        );

        // Oversized files are refused before decrypting.
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_DPAPI_BYTES + 1).unwrap();
        let err = s.get(&id, SecretKind::Password).unwrap_err();
        assert_eq!(err.to_string(), "Saved secrets are too large to read.");
    }

    #[test]
    fn dpapi_store_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<CodecStore<Fake>>();
    }

    /// The real DPAPI codec, only on Windows (CI).
    #[cfg(windows)]
    mod windows {
        use super::*;
        use crate::DpapiStore;

        #[test]
        fn dpapi_real_round_trip() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secrets.dpapi");
            let s = DpapiStore::new(path.clone());
            let (a, b) = (ProfileId::new(), ProfileId::new());
            s.set(&a, SecretKind::Password, SECRET).unwrap();
            s.set_many(&[
                entry(a, SecretKind::KeyPassphrase, "pp"),
                entry(b, SecretKind::Password, "pw-\u{e9}"),
            ])
            .unwrap();
            let again = DpapiStore::new(path.clone());
            assert_eq!(
                again.get(&a, SecretKind::Password).unwrap().as_deref(),
                Some(SECRET)
            );
            assert_eq!(again.entries(&[a, b]).unwrap().len(), 3);
            again.remove(&a).unwrap();
            assert!(again.entries(&[a]).unwrap().is_empty());
            assert_eq!(
                again.get(&b, SecretKind::Password).unwrap().as_deref(),
                Some("pw-\u{e9}")
            );
            assert_eq!(dir_names(dir.path()), vec!["secrets.dpapi".to_owned()]);

            // Tampered bytes do not decrypt.
            let mut bytes = std::fs::read(&path).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
            std::fs::write(&path, &bytes).unwrap();
            let err = again.get(&b, SecretKind::Password).unwrap_err();
            assert_eq!(
                err.to_string(),
                "Saved secrets could not be read on this Windows account."
            );
        }

        #[test]
        fn dpapi_file_never_contains_plaintext() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("secrets.dpapi");
            let s = DpapiStore::new(path.clone());
            let id = ProfileId::new();
            s.set(&id, SecretKind::Password, SECRET).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            let hay = String::from_utf8_lossy(&bytes);
            let utf16: Vec<u8> = SECRET.encode_utf16().flat_map(u16::to_le_bytes).collect();
            for needle in [SECRET, &id.to_string(), "password", "format", "secrets"] {
                assert!(!hay.contains(needle), "{needle} must not appear");
            }
            assert!(!bytes.windows(utf16.len()).any(|w| w == utf16.as_slice()));
            // The protected payload is the versioned document.
            let plain = super::super::Dpapi.unprotect(&bytes).unwrap();
            let text = std::str::from_utf8(&plain).unwrap();
            assert!(text.starts_with("format = 1\n"), "{}", text.len());
            assert!(text.contains(SECRET));
        }
    }
}
