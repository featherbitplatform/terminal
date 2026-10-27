//! SFTP on a profile's shared SSH connection, for the file panel.
//!
//! [`Sftp::open`] takes the profile's pooled [`Connection`] (dialling it, jump chain included,
//! when none is up) and opens an `sftp` subsystem channel on it. The [`Sftp`] holds the
//! connection, so it stays open while the panel or a transfer uses it, and closes its channel
//! when dropped. The channel is client-initiated: the handler's refusal of server-opened
//! channels is not involved.
//!
//! Errors are [`SftpError`]s whose text is a plain sentence fit to show inline in the panel.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fbterm_config::{ProfileId, ProfileStore};
use futures_util::StreamExt;
use russh::ChannelMsg;
use russh_sftp::client::error::Error as RawError;
use russh_sftp::client::rawsession::Limits;
use russh_sftp::client::{Config, RawSftpSession};
use russh_sftp::protocol::{FileAttributes, StatusCode};

use super::{Connection, HOP_TIMEOUT, Prompter, SshError, SshPool};

/// The extension that renames over an existing file atomically.
const POSIX_RENAME: &str = "posix-rename@openssh.com";
/// How many symlinks of one listing are resolved at once.
const LINK_LOOKUPS: usize = 16;

/// An SFTP session on a profile's shared connection. Requests may run concurrently.
pub struct Sftp {
    session: RawSftpSession,
    posix_rename: bool,
    /// What the server allows per packet, read and write (`limits@openssh.com`), if it said.
    #[allow(dead_code)]
    limits: Option<Limits>,
    /// Keeps the profile's connection (and its jump chain) open while the session lives.
    _connection: Arc<Connection>,
}

/// One item of a folder, or the answer to [`Sftp::stat`] / [`Sftp::lstat`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file name as the server sent it (invalid UTF-8 replaced), never interpreted.
    pub name: String,
    pub kind: EntryKind,
    /// Size in bytes; 0 when the server did not say.
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// The raw mode (file type and permission bits), when the server sent it.
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A symbolic link: its `target` as stored (empty when it could not be read) and what it
    /// ends at, `None` when that does not exist or cannot be reached (a broken link).
    Link {
        target: String,
        to: Option<Box<EntryKind>>,
    },
    /// Devices, sockets, pipes, and anything without a type.
    Other,
}

impl EntryKind {
    /// Whether it opens as a folder: a directory, or a link that ends at one.
    pub fn is_folder(&self) -> bool {
        match self {
            Self::Dir => true,
            Self::Link { to: Some(to), .. } => to.is_folder(),
            _ => false,
        }
    }
}

impl Entry {
    /// Whether it opens as a folder (see [`EntryKind::is_folder`]).
    pub fn is_folder(&self) -> bool {
        self.kind.is_folder()
    }
}

/// Why an SFTP request failed. The text is a plain sentence naming the path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SftpError {
    /// The session or its connection ended, or the server stopped answering.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Denied(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

/// The column a listing is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
}

/// Sort a listing: folders (see [`Entry::is_folder`]) always first, then everything else,
/// each group by `key` (ties by name), `descending` reversing within the groups. Names
/// compare without case, then with it; unknown modified times come before known ones.
pub fn sort_entries(entries: &mut [Entry], key: SortKey, descending: bool) {
    entries.sort_by(|a, b| {
        b.is_folder().cmp(&a.is_folder()).then_with(|| {
            let by_name = || {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.name.cmp(&b.name))
            };
            let order = match key {
                SortKey::Name => by_name(),
                SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
                SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            };
            if descending { order.reverse() } else { order }
        })
    });
}

/// The kind a mode's file type bits name; a link's target is filled in later.
fn kind_of_mode(mode: Option<u32>) -> EntryKind {
    const S_IFMT: u32 = 0o170000;
    match mode.map(|m| m & S_IFMT) {
        Some(0o100000) => EntryKind::File,
        Some(0o040000) => EntryKind::Dir,
        Some(0o120000) => EntryKind::Link {
            target: String::new(),
            to: None,
        },
        _ => EntryKind::Other,
    }
}

fn modified_of(attrs: &FileAttributes) -> Option<SystemTime> {
    attrs
        .mtime
        .map(|secs| UNIX_EPOCH + Duration::from_secs(u64::from(secs)))
}

/// `name` inside the folder `dir`.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The last component of `path` (the path itself for `/`).
fn base_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => path.to_owned(),
    }
}

/// `text` as a sentence: ends with a full stop.
fn sentence(text: String) -> String {
    if text.ends_with('.') {
        text
    } else {
        format!("{text}.")
    }
}

/// A request on `path` failed with `error`.
fn error_for(error: RawError, path: &str) -> SftpError {
    match error {
        RawError::Status(status) => {
            let detail = status.error_message.trim().to_owned();
            match status.status_code {
                StatusCode::NoSuchFile => SftpError::NotFound(format!("{path} does not exist.")),
                StatusCode::PermissionDenied => {
                    SftpError::Denied(format!("Permission denied for {path}."))
                }
                StatusCode::NoConnection | StatusCode::ConnectionLost => {
                    SftpError::Unavailable("The connection to the server was lost.".into())
                }
                code => {
                    let detail = if detail.is_empty() {
                        code.to_string()
                    } else {
                        detail
                    };
                    SftpError::Other(sentence(format!(
                        "The server could not complete the request for {path}: {detail}"
                    )))
                }
            }
        }
        RawError::Timeout => SftpError::Unavailable("The server did not answer in time.".into()),
        RawError::IO(_) => SftpError::Unavailable("The SFTP session has ended.".into()),
        RawError::UnexpectedBehavior(m) if m.contains("session closed") => {
            SftpError::Unavailable("The SFTP session has ended.".into())
        }
        other => SftpError::Other(sentence(format!(
            "The server sent an unexpected answer for {path}: {other}"
        ))),
    }
}

impl Sftp {
    /// Open an SFTP session on `profile`'s shared connection (dialling it through its jump
    /// chain if it is not up, asking `prompter` as [`SshPool::acquire`] does). A server that
    /// refuses the `sftp` subsystem fails with [`SshError::Protocol`] carrying the reason.
    pub async fn open(
        pool: &SshPool,
        store: &ProfileStore,
        profile: &ProfileId,
        prompter: Arc<dyn Prompter>,
    ) -> Result<Sftp, SshError> {
        let connection = pool.acquire(store, profile, prompter).await?;
        Self::on(connection).await
    }

    async fn on(connection: Arc<Connection>) -> Result<Sftp, SshError> {
        let unavailable = |what: &str| SshError::Protocol(what.to_owned());
        let start = async {
            let mut channel = connection.target().channel_open_session().await?;
            channel.request_subsystem(true, "sftp").await?;
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Success) => break,
                    Some(ChannelMsg::Failure) => {
                        return Err(unavailable("the server refused the sftp subsystem"));
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                        return Err(unavailable("the server closed the sftp channel"));
                    }
                    Some(_) => {}
                }
            }
            let config = Config {
                request_timeout_secs: HOP_TIMEOUT.as_secs(),
                ..Config::default()
            };
            let mut session = RawSftpSession::new_with_config(channel.into_stream(), config);
            let version = session
                .init()
                .await
                .map_err(|e| SshError::Protocol(format!("the sftp session did not start: {e}")))?;
            let has =
                |name: &str, ver: &str| version.extensions.get(name).is_some_and(|v| v == ver);
            let posix_rename = has(POSIX_RENAME, "1");
            let limits = if has(russh_sftp::extensions::LIMITS, "1") {
                let limits = Limits::from(session.limits().await.map_err(|e| {
                    SshError::Protocol(format!("the sftp session did not start: {e}"))
                })?);
                session.set_limits(limits);
                Some(limits)
            } else {
                None
            };
            Ok::<_, SshError>((session, posix_rename, limits))
        };
        let (session, posix_rename, limits) = tokio::time::timeout(HOP_TIMEOUT, start)
            .await
            .map_err(|_| unavailable("the server did not start the sftp session in time"))??;
        Ok(Sftp {
            session,
            posix_rename,
            limits,
            _connection: connection,
        })
    }

    /// The home folder: the server's answer for `.` (`/` in some chroots).
    pub async fn home(&self) -> Result<String, SftpError> {
        self.realpath(".").await
    }

    /// The absolute, normalised form of `path` as the server resolves it.
    pub async fn realpath(&self, path: &str) -> Result<String, SftpError> {
        let name = self
            .session
            .realpath(path)
            .await
            .map_err(|e| error_for(e, path))?;
        name.files
            .into_iter()
            .next()
            .map(|f| f.filename)
            .ok_or_else(|| SftpError::Other(format!("The server gave no answer for {path}.")))
    }

    /// The folder `path`, without `.` and `..`, folders first then the rest by name. Links
    /// carry their target and what they end at.
    pub async fn list(&self, path: &str) -> Result<Vec<Entry>, SftpError> {
        let err = |e| error_for(e, path);
        let handle = self.session.opendir(path).await.map_err(err)?.handle;
        let mut raw = Vec::new();
        let read = loop {
            match self.session.readdir(handle.as_str()).await {
                Ok(name) => raw.extend(
                    name.files
                        .into_iter()
                        .filter(|f| f.filename != "." && f.filename != "..")
                        .map(|f| (f.filename, f.attrs)),
                ),
                Err(RawError::Status(s)) if s.status_code == StatusCode::Eof => break Ok(()),
                Err(e) => break Err(err(e)),
            }
        };
        let _ = self.session.close(handle).await;
        read?;
        let mut entries: Vec<Entry> = futures_util::stream::iter(raw)
            .map(|(name, attrs)| {
                let full = join(path, &name);
                async move { self.entry(name, &full, &attrs).await }
            })
            .buffered(LINK_LOOKUPS)
            .collect()
            .await;
        sort_entries(&mut entries, SortKey::Name, false);
        Ok(entries)
    }

    /// `path`, following links (so its kind is never [`EntryKind::Link`]).
    pub async fn stat(&self, path: &str) -> Result<Entry, SftpError> {
        let attrs = self
            .session
            .stat(path)
            .await
            .map_err(|e| error_for(e, path))?
            .attrs;
        Ok(self.entry(base_name(path), path, &attrs).await)
    }

    /// `path` itself: a link is reported as one, with its target.
    pub async fn lstat(&self, path: &str) -> Result<Entry, SftpError> {
        let attrs = self
            .session
            .lstat(path)
            .await
            .map_err(|e| error_for(e, path))?
            .attrs;
        Ok(self.entry(base_name(path), path, &attrs).await)
    }

    /// Whether the server can rename over an existing file atomically
    /// (`posix-rename@openssh.com`).
    pub fn supports_posix_rename(&self) -> bool {
        self.posix_rename
    }

    /// The entry `name` at `full` with `attrs`; a link gets its target and what it ends at.
    async fn entry(&self, name: String, full: &str, attrs: &FileAttributes) -> Entry {
        let mut kind = kind_of_mode(attrs.permissions);
        if let EntryKind::Link { .. } = kind {
            let (target, end) =
                futures_util::future::join(self.session.readlink(full), self.session.stat(full))
                    .await;
            let target = target
                .ok()
                .and_then(|n| n.files.into_iter().next())
                .map(|f| f.filename)
                .unwrap_or_default();
            let to = end
                .ok()
                .map(|a| Box::new(kind_of_mode(a.attrs.permissions)));
            kind = EntryKind::Link { target, to };
        }
        Entry {
            name,
            kind,
            size: attrs.size.unwrap_or(0),
            modified: modified_of(attrs),
            mode: attrs.permissions,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn e(name: &str, kind: EntryKind, size: u64, modified: Option<u64>) -> Entry {
        Entry {
            name: name.into(),
            kind,
            size,
            modified: modified.map(|s| UNIX_EPOCH + Duration::from_secs(s)),
            mode: None,
        }
    }

    fn link(to: Option<EntryKind>) -> EntryKind {
        EntryKind::Link {
            target: "t".into(),
            to: to.map(Box::new),
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn entry_sorting_helpers() {
        // Folders are directories and links that end at one; nothing else opens as a folder.
        assert!(EntryKind::Dir.is_folder());
        assert!(link(Some(EntryKind::Dir)).is_folder());
        assert!(!link(Some(EntryKind::File)).is_folder());
        assert!(!link(None).is_folder());
        assert!(!EntryKind::File.is_folder());
        assert!(!EntryKind::Other.is_folder());

        let mut entries = vec![
            e("zeta.txt", EntryKind::File, 30, Some(300)),
            e("Beta", EntryKind::Dir, 4096, Some(100)),
            e("alpha.log", EntryKind::File, 10, None),
            e("gamma", link(Some(EntryKind::Dir)), 7, Some(50)),
            e("Delta.md", EntryKind::File, 20, Some(200)),
            e("broken", link(None), 6, Some(400)),
            e("Alpha", EntryKind::Dir, 4096, Some(500)),
        ];

        // By name: folders first, then the rest; case-insensitive, upper case first on a tie.
        sort_entries(&mut entries, SortKey::Name, false);
        assert_eq!(
            names(&entries),
            [
                "Alpha",
                "Beta",
                "gamma",
                "alpha.log",
                "broken",
                "Delta.md",
                "zeta.txt"
            ]
        );
        // Reversed: each group is reversed, folders stay first.
        sort_entries(&mut entries, SortKey::Name, true);
        assert_eq!(
            names(&entries),
            [
                "gamma",
                "Beta",
                "Alpha",
                "zeta.txt",
                "Delta.md",
                "broken",
                "alpha.log"
            ]
        );
        // By size, ties broken by name.
        sort_entries(&mut entries, SortKey::Size, false);
        assert_eq!(
            names(&entries),
            [
                "gamma",
                "Alpha",
                "Beta",
                "broken",
                "alpha.log",
                "Delta.md",
                "zeta.txt"
            ]
        );
        // By modified time, unknown times first.
        sort_entries(&mut entries, SortKey::Modified, false);
        assert_eq!(
            names(&entries),
            [
                "gamma",
                "Beta",
                "Alpha",
                "alpha.log",
                "Delta.md",
                "zeta.txt",
                "broken"
            ]
        );
        sort_entries(&mut entries, SortKey::Modified, true);
        assert_eq!(
            names(&entries),
            [
                "Alpha",
                "Beta",
                "gamma",
                "broken",
                "zeta.txt",
                "Delta.md",
                "alpha.log"
            ]
        );
    }

    #[test]
    fn kind_comes_from_the_mode_bits() {
        assert_eq!(kind_of_mode(Some(0o100644)), EntryKind::File);
        assert_eq!(kind_of_mode(Some(0o040755)), EntryKind::Dir);
        assert!(matches!(
            kind_of_mode(Some(0o120777)),
            EntryKind::Link { .. }
        ));
        assert_eq!(kind_of_mode(Some(0o010644)), EntryKind::Other);
        assert_eq!(kind_of_mode(Some(0o140755)), EntryKind::Other);
        assert_eq!(kind_of_mode(None), EntryKind::Other);
    }
}
