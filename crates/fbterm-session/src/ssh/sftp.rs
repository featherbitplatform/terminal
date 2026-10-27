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
use futures_util::{StreamExt, TryStreamExt};
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
/// The most entries one listing reads before giving up on the folder.
const MAX_ENTRIES: usize = 100_000;
/// How long one listing (reading the folder and resolving its links) may take in all.
const LIST_DEADLINE: Duration = Duration::from_secs(60);
/// How long closing a folder handle may take before the listing stops waiting for it.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// An SFTP session on a profile's shared connection. Requests may run concurrently.
pub struct Sftp {
    session: RawSftpSession,
    posix_rename: bool,
    /// What the server allows per packet, read and write (`limits@openssh.com`), if it said.
    #[allow(dead_code)]
    limits: Option<Limits>,
    /// Keeps the profile's connection (and its jump chain) open while the session lives
    /// (`None` only in unit tests, which run the session over an in-memory stream).
    _connection: Option<Arc<Connection>>,
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
pub fn sort_entries(entries: &mut Vec<Entry>, key: SortKey, descending: bool) {
    // Lowercase names are computed once per entry, not on every comparison.
    let mut keyed: Vec<(String, Entry)> = std::mem::take(entries)
        .into_iter()
        .map(|e| (e.name.to_lowercase(), e))
        .collect();
    keyed.sort_by(|(la, a), (lb, b)| {
        b.is_folder().cmp(&a.is_folder()).then_with(|| {
            let by_name = || la.cmp(lb).then_with(|| a.name.cmp(&b.name));
            let order = match key {
                SortKey::Name => by_name(),
                SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
                SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            };
            if descending { order.reverse() } else { order }
        })
    });
    entries.extend(keyed.into_iter().map(|(_, e)| e));
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
            Self::start(channel.into_stream(), Some(connection.clone())).await
        };
        tokio::time::timeout(HOP_TIMEOUT, start)
            .await
            .map_err(|_| unavailable("the server did not start the sftp session in time"))?
    }

    /// Start the SFTP protocol on `stream` (the subsystem channel) and read the server's
    /// extensions.
    async fn start<S>(stream: S, connection: Option<Arc<Connection>>) -> Result<Sftp, SshError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let config = Config {
            request_timeout_secs: HOP_TIMEOUT.as_secs(),
            ..Config::default()
        };
        let mut session = RawSftpSession::new_with_config(stream, config);
        {
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
            Ok(Sftp {
                session,
                posix_rename,
                limits,
                _connection: connection,
            })
        }
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
        self.list_within(path, MAX_ENTRIES, LIST_DEADLINE).await
    }

    /// [`Sftp::list`] with its limits as parameters, so tests can lower them.
    async fn list_within(
        &self,
        path: &str,
        max_entries: usize,
        deadline: Duration,
    ) -> Result<Vec<Entry>, SftpError> {
        let deadline = tokio::time::Instant::now() + deadline;
        let too_slow =
            || SftpError::Unavailable(format!("The server took too long to list {path}."));
        let handle = tokio::time::timeout_at(deadline, self.session.opendir(path))
            .await
            .map_err(|_| too_slow())?
            .map_err(|e| error_for(e, path))?
            .handle;
        let read = async {
            let mut raw = Vec::new();
            loop {
                match self.session.readdir(handle.as_str()).await {
                    Ok(name) => {
                        raw.extend(
                            name.files
                                .into_iter()
                                .filter(|f| f.filename != "." && f.filename != "..")
                                .map(|f| (f.filename, f.attrs)),
                        );
                        if raw.len() > max_entries {
                            return Err(SftpError::Other(
                                "This folder has too many entries to list.".into(),
                            ));
                        }
                    }
                    Err(RawError::Status(s)) if s.status_code == StatusCode::Eof => {
                        return Ok(raw);
                    }
                    Err(e) => return Err(error_for(e, path)),
                }
            }
        };
        let raw = tokio::time::timeout_at(deadline, read).await;
        // Every way out of the read closes the handle (best effort, bounded). A caller that
        // drops the listing mid-read leaves it open on the server until the session ends.
        let _ = tokio::time::timeout(CLOSE_WAIT, self.session.close(handle)).await;
        let raw = raw.map_err(|_| too_slow())??;
        let resolve = futures_util::stream::iter(raw)
            .map(|(name, attrs)| {
                let full = join(path, &name);
                async move { self.entry(name, &full, &attrs).await }
            })
            .buffered(LINK_LOOKUPS)
            .try_collect::<Vec<Entry>>();
        let mut entries = tokio::time::timeout_at(deadline, resolve)
            .await
            .map_err(|_| too_slow())??;
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
        self.entry(base_name(path), path, &attrs).await
    }

    /// `path` itself: a link is reported as one, with its target.
    pub async fn lstat(&self, path: &str) -> Result<Entry, SftpError> {
        let attrs = self
            .session
            .lstat(path)
            .await
            .map_err(|e| error_for(e, path))?
            .attrs;
        self.entry(base_name(path), path, &attrs).await
    }

    /// Whether the server can rename over an existing file atomically
    /// (`posix-rename@openssh.com`).
    pub fn supports_posix_rename(&self) -> bool {
        self.posix_rename
    }

    /// The entry `name` at `full` with `attrs`; a link gets its target and what it ends at.
    /// A link whose target cannot be read or reached shows as broken; only a session that
    /// ended (or stopped answering) fails.
    async fn entry(
        &self,
        name: String,
        full: &str,
        attrs: &FileAttributes,
    ) -> Result<Entry, SftpError> {
        let mut kind = kind_of_mode(attrs.permissions);
        if let EntryKind::Link { .. } = kind {
            let (target, end) =
                futures_util::future::join(self.session.readlink(full), self.session.stat(full))
                    .await;
            let target = match target.map_err(|e| error_for(e, full)) {
                Ok(n) => n
                    .files
                    .into_iter()
                    .next()
                    .map(|f| f.filename)
                    .unwrap_or_default(),
                Err(e @ SftpError::Unavailable(_)) => return Err(e),
                Err(_) => String::new(),
            };
            let to = match end.map_err(|e| error_for(e, full)) {
                Ok(a) => Some(Box::new(kind_of_mode(a.attrs.permissions))),
                Err(e @ SftpError::Unavailable(_)) => return Err(e),
                Err(_) => None,
            };
            kind = EntryKind::Link { target, to };
        }
        Ok(Entry {
            name,
            kind,
            size: attrs.size.unwrap_or(0),
            modified: modified_of(attrs),
            mode: attrs.permissions,
        })
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

    /// `fut`, failing the test instead of hanging when it never finishes.
    async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(10), fut)
            .await
            .expect("the listing never finished")
    }

    fn link_kind(entry: &Entry) -> &EntryKind {
        &entry.kind
    }

    #[tokio::test]
    async fn unreachable_link_targets_show_as_broken() {
        for code in [
            StatusCode::NoSuchFile,
            StatusCode::PermissionDenied,
            StatusCode::Failure,
        ] {
            let sftp = fake::session(fake::Server::new(fake::Dir::OneLink, Some(code))).await;
            let entries = sftp.list("/d").await.unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(
                link_kind(&entries[0]),
                &EntryKind::Link {
                    target: "target".into(),
                    to: None,
                },
                "{code:?}"
            );
        }
        let sftp = fake::session(fake::Server::new(fake::Dir::OneLink, None)).await;
        let entries = sftp.list("/d").await.unwrap();
        assert_eq!(
            link_kind(&entries[0]),
            &EntryKind::Link {
                target: "target".into(),
                to: Some(Box::new(EntryKind::File)),
            }
        );
    }

    #[tokio::test]
    async fn lost_connection_while_resolving_links_fails_the_listing() {
        let server = fake::Server::new(fake::Dir::OneLink, Some(StatusCode::ConnectionLost));
        let sftp = fake::session(server).await;
        let err = sftp.list("/d").await.unwrap_err();
        assert!(matches!(err, SftpError::Unavailable(_)), "{err:?}");
        let err = sftp.lstat("/d/link").await.unwrap_err();
        assert!(matches!(err, SftpError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn endless_folder_stops_at_the_entry_cap() {
        let server = fake::Server::new(fake::Dir::Endless, None);
        let sftp = fake::session(server.clone()).await;
        let err = bounded(sftp.list_within("/d", 95, Duration::from_secs(60)))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SftpError::Other("This folder has too many entries to list.".into())
        );
        assert_eq!(server.closes(), 1, "the folder handle is closed");
    }

    #[tokio::test]
    async fn slow_folder_stops_at_the_deadline() {
        let server = fake::Server::new(fake::Dir::Slow(Duration::from_millis(20)), None);
        let sftp = fake::session(server.clone()).await;
        let started = std::time::Instant::now();
        let err = bounded(sftp.list_within("/d", MAX_ENTRIES, Duration::from_millis(300)))
            .await
            .unwrap_err();
        assert!(matches!(err, SftpError::Unavailable(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(server.closes(), 1, "the folder handle is closed");
    }

    #[test]
    fn default_listing_limits() {
        assert_eq!(MAX_ENTRIES, 100_000);
        assert_eq!(LIST_DEADLINE, Duration::from_secs(60));
    }

    /// An in-memory SFTP server that misbehaves on purpose.
    mod fake {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        use russh_sftp::protocol::{Attrs, File, FileAttributes, Handle, Name, Status, StatusCode};
        use russh_sftp::server::{Handler, StatusReply};

        use super::super::Sftp;

        #[derive(Clone, Copy)]
        pub enum Dir {
            /// One symlink, then the end of the folder.
            OneLink,
            /// A batch of files on every read, never the end.
            Endless,
            /// One file per read after `delay`, never the end.
            Slow(Duration),
        }

        #[derive(Clone)]
        pub struct Server {
            pub dir: Dir,
            /// What `stat` and `readlink` answer for the link.
            pub link_status: Option<StatusCode>,
            pub closes: Arc<AtomicUsize>,
            pub reads: usize,
        }

        impl Server {
            pub fn new(dir: Dir, link_status: Option<StatusCode>) -> Self {
                Self {
                    dir,
                    link_status,
                    closes: Arc::default(),
                    reads: 0,
                }
            }

            pub fn closes(&self) -> usize {
                self.closes.load(Ordering::SeqCst)
            }
        }

        fn mode_attrs(mode: u32) -> FileAttributes {
            FileAttributes {
                permissions: Some(mode),
                size: Some(1),
                ..FileAttributes::default()
            }
        }

        impl Handler for Server {
            type Error = StatusReply;

            fn unimplemented(&self) -> Self::Error {
                StatusCode::OpUnsupported.into()
            }

            async fn opendir(&mut self, id: u32, _path: String) -> Result<Handle, Self::Error> {
                Ok(Handle {
                    id,
                    handle: "h".into(),
                })
            }

            async fn readdir(&mut self, id: u32, _handle: String) -> Result<Name, Self::Error> {
                match self.dir {
                    Dir::OneLink => {
                        self.reads += 1;
                        if self.reads == 1 {
                            Ok(Name {
                                id,
                                files: vec![File::new("link", mode_attrs(0o120777))],
                            })
                        } else {
                            Err(StatusCode::Eof.into())
                        }
                    }
                    Dir::Endless => Ok(Name {
                        id,
                        files: (0..10)
                            .map(|i| File::new(format!("f{i}"), mode_attrs(0o100644)))
                            .collect(),
                    }),
                    Dir::Slow(delay) => {
                        tokio::time::sleep(delay).await;
                        Ok(Name {
                            id,
                            files: vec![File::new("f", mode_attrs(0o100644))],
                        })
                    }
                }
            }

            async fn close(&mut self, id: u32, _handle: String) -> Result<Status, Self::Error> {
                self.closes.fetch_add(1, Ordering::SeqCst);
                Ok(Status {
                    id,
                    status_code: StatusCode::Ok,
                    error_message: "Ok".into(),
                    language_tag: "en-US".into(),
                })
            }

            async fn lstat(&mut self, id: u32, _path: String) -> Result<Attrs, Self::Error> {
                Ok(Attrs {
                    id,
                    attrs: mode_attrs(0o120777),
                })
            }

            async fn stat(&mut self, id: u32, _path: String) -> Result<Attrs, Self::Error> {
                match self.link_status {
                    Some(code) => Err(code.into()),
                    None => Ok(Attrs {
                        id,
                        attrs: mode_attrs(0o100644),
                    }),
                }
            }

            async fn readlink(&mut self, id: u32, _path: String) -> Result<Name, Self::Error> {
                match self.link_status {
                    Some(StatusCode::ConnectionLost) => Err(StatusCode::ConnectionLost.into()),
                    _ => Ok(Name {
                        id,
                        files: vec![File::dummy("target")],
                    }),
                }
            }
        }

        /// A session talking to `server` over an in-memory pipe.
        pub async fn session(server: Server) -> Sftp {
            let (client, remote) = tokio::io::duplex(64 * 1024);
            russh_sftp::server::run(remote, server).await;
            Sftp::start(client, None).await.expect("fake sftp session")
        }
    }
}
