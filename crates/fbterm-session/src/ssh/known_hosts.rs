//! Reading and extending an OpenSSH `known_hosts` file.
//!
//! Supported per line: plain and `[host]:port` names, comma-separated name lists, `*`/`?`
//! wildcards, `!` negation, hashed names (`|1|salt|hash`), `@revoked` (a revoked key counts as
//! changed) and `@cert-authority` (ignored: host certificates are not supported). Lines that do
//! not parse are skipped, never fatal.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use data_encoding::BASE64;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{Algorithm, PublicKey};
use sha1::Sha1;

use super::SshError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// The key is recorded for this host.
    Known,
    /// Nothing is recorded for this host.
    Unknown,
    /// Keys are recorded for this host but none is this one (or this one is revoked).
    Changed,
}

/// The name a host is recorded under: `host`, or `[host]:port` when the port is not 22.
fn lookup_name(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

enum Marker {
    None,
    Revoked,
}

struct Entry {
    marker: Marker,
    key: PublicKey,
}

/// Every parseable entry whose host pattern matches `name`.
fn matching_entries(text: &str, name: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(mut first) = fields.next() else {
            continue;
        };
        let marker = match first {
            "@revoked" => Marker::Revoked,
            // Certificate authorities vouch for host certificates, which are not supported.
            "@cert-authority" => continue,
            m if m.starts_with('@') => continue,
            _ => Marker::None,
        };
        if !matches!(marker, Marker::None) {
            match fields.next() {
                Some(f) => first = f,
                None => continue,
            }
        }
        let (Some(kind), Some(data)) = (fields.next(), fields.next()) else {
            continue;
        };
        if !hosts_match(first, name) {
            continue;
        }
        match PublicKey::from_openssh(&format!("{kind} {data}")) {
            Ok(key) => out.push(Entry { marker, key }),
            Err(_) => tracing::debug!("skipping unparseable known_hosts entry"),
        }
    }
    out
}

/// Whether a comma-separated host pattern list matches `name` (already lower-case).
fn hosts_match(patterns: &str, name: &str) -> bool {
    let mut matched = false;
    for pattern in patterns.split(',') {
        if let Some(negated) = pattern.strip_prefix('!') {
            if glob(&negated.to_ascii_lowercase(), name) {
                return false;
            }
        } else if pattern.starts_with("|1|") {
            matched |= hashed_match(pattern, name);
        } else {
            matched |= glob(&pattern.to_ascii_lowercase(), name);
        }
    }
    matched
}

/// `|1|base64(salt)|base64(HMAC-SHA1(salt, name))`.
fn hashed_match(pattern: &str, name: &str) -> bool {
    let mut parts = pattern["|1|".len()..].split('|');
    let (Some(salt), Some(hash), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (
        BASE64.decode(salt.as_bytes()),
        BASE64.decode(hash.as_bytes()),
    ) else {
        return false;
    };
    let Ok(mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else {
        return false;
    };
    mac.chain_update(name.as_bytes())
        .verify_slice(&hash)
        .is_ok()
}

/// OpenSSH wildcards: `*` any run of characters, `?` exactly one.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn read_known_hosts(path: &Path) -> Result<String, SshError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(SshError::Protocol(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Characters that would change the meaning of a `known_hosts` line if they were part of a
/// host name: list separators, wildcards, negation, brackets, comments, markers and hashes.
const HOST_SPECIALS: &str = ",*?![]#@|";

/// Refuse host names that cannot be written to (or looked up in) `known_hosts` literally.
fn validate_host(host: &str) -> Result<(), SshError> {
    if host.is_empty() {
        return Err(SshError::Protocol("empty host name".into()));
    }
    if let Some(c) = host
        .chars()
        .find(|&c| c.is_control() || c.is_whitespace() || HOST_SPECIALS.contains(c))
    {
        return Err(SshError::Protocol(format!(
            "invalid host name {host:?}: it contains {c:?}, which is not allowed in a host name"
        )));
    }
    Ok(())
}

/// Classify `key` for `host`:`port` against the `known_hosts` file at `known_hosts`.
///
/// Stricter than "same algorithm" matching: if any key is recorded for the host and `key` is
/// not one of them, the result is [`HostKeyStatus::Changed`]. Connections prefer the recorded
/// key types (see `known_algorithms`), so a legitimate server presents a recorded key.
pub fn check_host_key(
    known_hosts: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<HostKeyStatus, SshError> {
    validate_host(host)?;
    let text = read_known_hosts(known_hosts)?;
    let entries = matching_entries(&text, &lookup_name(host, port));
    let same = |e: &Entry| e.key.key_data() == key.key_data();
    if entries
        .iter()
        .any(|e| matches!(e.marker, Marker::Revoked) && same(e))
    {
        return Ok(HostKeyStatus::Changed);
    }
    let recorded: Vec<&Entry> = entries
        .iter()
        .filter(|e| matches!(e.marker, Marker::None))
        .collect();
    Ok(if recorded.iter().any(|e| same(e)) {
        HostKeyStatus::Known
    } else if recorded.is_empty() {
        HostKeyStatus::Unknown
    } else {
        HostKeyStatus::Changed
    })
}

/// Algorithms of the keys recorded for `host`:`port` (empty if none or unreadable).
pub(crate) fn known_algorithms(known_hosts: &Path, host: &str, port: u16) -> Vec<Algorithm> {
    if validate_host(host).is_err() {
        return Vec::new();
    }
    let Ok(text) = read_known_hosts(known_hosts) else {
        return Vec::new();
    };
    let mut algs = Vec::new();
    for entry in matching_entries(&text, &lookup_name(host, port)) {
        if matches!(entry.marker, Marker::None) {
            let alg = entry.key.algorithm();
            // An RSA key can be offered with any RSA signature hash; prefer the strongest.
            let variants = if matches!(alg, Algorithm::Rsa { .. }) {
                vec![
                    Algorithm::Rsa {
                        hash: Some(russh::keys::HashAlg::Sha512),
                    },
                    Algorithm::Rsa {
                        hash: Some(russh::keys::HashAlg::Sha256),
                    },
                    Algorithm::Rsa { hash: None },
                ]
            } else {
                vec![alg]
            };
            for a in variants {
                if !algs.contains(&a) {
                    algs.push(a);
                }
            }
        }
    }
    algs
}

/// Append `key` for `host`:`port` as one OpenSSH-format line, creating the directory (0700)
/// and file (0600) if needed. Existing lines are never rewritten.
pub fn learn_host_key(
    known_hosts: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<(), SshError> {
    validate_host(host)?;
    let io = |e: std::io::Error| {
        SshError::Protocol(format!("cannot write {}: {e}", known_hosts.display()))
    };
    // `algorithm base64`, without the key's comment.
    let openssh = key
        .to_openssh()
        .map_err(|e| SshError::Protocol(format!("cannot encode host key: {e}")))?;
    let key_text = openssh
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");

    if let Some(dir) = known_hosts.parent().filter(|d| !d.as_os_str().is_empty())
        && !dir.exists()
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).map_err(io)?;
    }

    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(known_hosts).map_err(io)?;

    let mut line = String::new();
    if file.seek(SeekFrom::End(-1)).is_ok() {
        let mut last = [0u8; 1];
        file.read_exact(&mut last).map_err(io)?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    line.push_str(&lookup_name(host, port));
    line.push(' ');
    line.push_str(&key_text);
    line.push('\n');
    // One write in append mode: the line lands whole at the end of the file.
    file.write_all(line.as_bytes()).map_err(io)?;
    file.flush().map_err(io)
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// `ssh-keygen`-generated test keys (public halves only).
    pub const ED_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII86HwADXRc252S/lshVQ14LamZk0KG7eRmpkTH7ZoEA";
    pub const ED_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJv7NQC+tDZgzmdtiZkntHZzl6zKEQu4jGdangctDHtU";
    pub const ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBMiZ1j+XP9ErgzDocO0+yJFkU+29FMgNJPYMZuhv0ejHUZunPu7vEtRIBIu8VZdpaTuVoSFcthcF3b3QHDBFI9A=";
    /// `ssh-keygen -H` of `example.com ED_A` and `[example.com]:2200 ED_B`.
    pub const HASHED: &str = "\
|1|TcqRI0sQ/bKdKDVs6hlGTKdlNPw=|jshtAIoq0fjrRvpdf60WNs9r59Y= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII86HwADXRc252S/lshVQ14LamZk0KG7eRmpkTH7ZoEA
|1|vyBbKm8Ld3rJV0rcMdt9+PR6i1o=|JF7HFkvGmuVoSNh07qd3oUMaiJY= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJv7NQC+tDZgzmdtiZkntHZzl6zKEQu4jGdangctDHtU
";
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;

    fn key(s: &str) -> PublicKey {
        PublicKey::from_openssh(s).unwrap()
    }

    struct Kh {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    fn kh(contents: &str) -> Kh {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, contents).unwrap();
        Kh { _dir: dir, path }
    }

    fn status(f: &Kh, host: &str, port: u16, k: &str) -> HostKeyStatus {
        check_host_key(&f.path, host, port, &key(k)).unwrap()
    }

    #[test]
    fn plain_entry_on_default_port() {
        let f = kh(&format!("# comment\n\nexample.com {ED_A} some comment\n"));
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Known);
        assert_eq!(status(&f, "EXAMPLE.com", 22, ED_A), HostKeyStatus::Known);
        assert_eq!(status(&f, "example.com", 22, ED_B), HostKeyStatus::Changed);
        // The bare name only covers port 22.
        assert_eq!(
            status(&f, "example.com", 2200, ED_A),
            HostKeyStatus::Unknown
        );
        assert_eq!(status(&f, "other.com", 22, ED_A), HostKeyStatus::Unknown);
    }

    #[test]
    fn bracketed_entry_with_port() {
        let f = kh(&format!("[example.com]:2200\t{ED_A}\n"));
        assert_eq!(status(&f, "example.com", 2200, ED_A), HostKeyStatus::Known);
        assert_eq!(
            status(&f, "example.com", 2200, ED_B),
            HostKeyStatus::Changed
        );
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Unknown);
        assert_eq!(
            status(&f, "example.com", 2201, ED_A),
            HostKeyStatus::Unknown
        );
    }

    #[test]
    fn name_lists_wildcards_and_negation() {
        let f = kh(&format!(
            "alias,example.com,10.0.0.1 {ED_A}\n*.corp,!bad.corp {ED_B}\n"
        ));
        assert_eq!(status(&f, "10.0.0.1", 22, ED_A), HostKeyStatus::Known);
        assert_eq!(status(&f, "alias", 22, ED_A), HostKeyStatus::Known);
        assert_eq!(status(&f, "db.corp", 22, ED_B), HostKeyStatus::Known);
        assert_eq!(status(&f, "db.corp", 22, ED_A), HostKeyStatus::Changed);
        assert_eq!(status(&f, "bad.corp", 22, ED_B), HostKeyStatus::Unknown);
    }

    #[test]
    fn hashed_entries_match() {
        let f = kh(HASHED);
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Known);
        assert_eq!(status(&f, "example.com", 2200, ED_B), HostKeyStatus::Known);
        assert_eq!(status(&f, "example.com", 22, ED_B), HostKeyStatus::Changed);
        assert_eq!(
            status(&f, "example.com", 2200, ED_A),
            HostKeyStatus::Changed
        );
        assert_eq!(status(&f, "example.org", 22, ED_A), HostKeyStatus::Unknown);
    }

    #[test]
    fn any_recorded_key_of_another_type_means_changed() {
        let f = kh(&format!("example.com {ECDSA}\n"));
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Changed);
        assert_eq!(status(&f, "example.com", 22, ECDSA), HostKeyStatus::Known);
    }

    #[test]
    fn several_keys_for_one_host() {
        let f = kh(&format!("example.com {ECDSA}\nexample.com {ED_A}\n"));
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Known);
        assert_eq!(status(&f, "example.com", 22, ECDSA), HostKeyStatus::Known);
        assert_eq!(status(&f, "example.com", 22, ED_B), HostKeyStatus::Changed);
    }

    #[test]
    fn revoked_key_is_refused_and_cert_authority_ignored() {
        let f = kh(&format!(
            "example.com {ED_A}\n@revoked * {ED_A}\n@cert-authority *.corp {ED_B}\n"
        ));
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Changed);
        assert_eq!(status(&f, "db.corp", 22, ED_B), HostKeyStatus::Unknown);
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let f = kh(&format!(
            "example.com ssh-ed25519 not-base64!!\nincomplete\nexample.com {ED_A}\n"
        ));
        assert_eq!(status(&f, "example.com", 22, ED_A), HostKeyStatus::Known);
    }

    #[test]
    fn missing_file_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope");
        assert_eq!(
            check_host_key(&path, "example.com", 22, &key(ED_A)).unwrap(),
            HostKeyStatus::Unknown
        );
    }

    #[test]
    fn learn_appends_openssh_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh").join("known_hosts");
        let mut with_comment = key(ED_A);
        with_comment.set_comment("user@somewhere");

        learn_host_key(&path, "Example.com", 22, &with_comment).unwrap();
        learn_host_key(&path, "10.0.0.1", 2200, &key(ED_B)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            format!("example.com {ED_A}\n[10.0.0.1]:2200 {ED_B}\n")
        );
        assert_eq!(
            status(&Kh { _dir: dir, path }, "example.com", 22, ED_A),
            HostKeyStatus::Known
        );
    }

    #[test]
    fn learn_never_rewrites_existing_lines() {
        let original = format!("# mine\nexample.com {ED_A}"); // no trailing newline
        let f = kh(&original);
        learn_host_key(&f.path, "other.com", 22, &key(ED_B)).unwrap();
        let text = std::fs::read_to_string(&f.path).unwrap();
        assert_eq!(text, format!("{original}\nother.com {ED_B}\n"));
    }

    #[cfg(unix)]
    #[test]
    fn learn_creates_private_dir_and_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh").join("known_hosts");
        learn_host_key(&path, "example.com", 22, &key(ED_A)).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn known_algorithms_lists_recorded_types() {
        let f = kh(&format!("[h]:2200 {ECDSA}\n[h]:2200 {ED_A}\nh {ED_B}\n"));
        assert_eq!(
            known_algorithms(&f.path, "h", 2200),
            vec![
                Algorithm::Ecdsa {
                    curve: russh::keys::EcdsaCurve::NistP256
                },
                Algorithm::Ed25519
            ]
        );
        assert!(known_algorithms(&f.path, "x", 22).is_empty());
    }

    #[test]
    fn fingerprint_is_openssh_style() {
        assert_eq!(
            key(ED_A)
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string(),
            "SHA256:ZZ6opCTesVHgmgy+v+LpL6WlhnJ74SrzdAZofsEfPIA"
        );
    }

    #[test]
    fn host_names_that_would_change_a_line_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let k = key(ED_A);
        for bad in [
            "", "a b", "a\tb", "a\nb", "a\rb", "a\u{7f}b", "h,evil", "*", "h?", "!h", "[h]",
            "h]:22", "#h", "@revoked", "|1|x|y", "h\u{0}",
        ] {
            for result in [
                check_host_key(&path, bad, 22, &k).map(|_| ()),
                learn_host_key(&path, bad, 22, &k),
            ] {
                match result {
                    Err(SshError::Protocol(m)) => assert!(m.contains("host name"), "{bad:?}: {m}"),
                    other => panic!("{bad:?} accepted: {other:?}"),
                }
            }
            assert!(known_algorithms(&path, bad, 22).is_empty());
        }
        assert!(!path.exists(), "nothing may be written for a refused host");
        // Ordinary names and addresses still work.
        for good in ["example.com", "10.0.0.1", "::1", "fe80::1%eth0", "host-1_a"] {
            learn_host_key(&path, good, 2200, &k).unwrap();
            assert_eq!(
                check_host_key(&path, good, 2200, &k).unwrap(),
                HostKeyStatus::Known
            );
        }
    }
}
