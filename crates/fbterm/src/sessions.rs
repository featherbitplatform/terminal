//! Session lifecycle: starting local and SSH sessions, their events and terminal input.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use fbterm_config::{Profile, ProfileKind};
use fbterm_session::ssh::SshError;
use fbterm_session::{SessionEvent, SessionHandle, SessionId, spawn_local_into};
use fbterm_term::{GridSize, SharedTerminal, Terminal, TerminalOutput};
use iced::Task;
use iced::futures::Stream;
use iced::window;
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, DEFAULT_GRID, Delivered, Message};
use crate::prompts::UiPrompter;
use crate::tabs::{self, InputRoute, Tab, TabId, TabState, WindowState};

type Events = UnboundedReceiver<(SessionId, SessionEvent)>;

/// Session events from every session, delivered to the update loop by one subscription.
#[derive(Clone)]
pub(crate) struct EventReceiver(pub(crate) Arc<Mutex<Option<Events>>>);

impl Hash for EventReceiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "fbterm-session-events".hash(state);
    }
}

pub(crate) fn session_stream(
    rx: &EventReceiver,
) -> impl Stream<Item = (SessionId, SessionEvent)> + use<> {
    let rx = rx.0.lock().take();
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let event = rx.recv().await?;
        Some((event, Some(rx)))
    })
}

impl App {
    pub(crate) fn new_terminal(&self, grid: GridSize) -> SharedTerminal {
        let mut term = Terminal::new(grid, self.settings.scrollback_lines);
        self.style.scheme.apply_to(&mut term);
        Arc::new(Mutex::new(term))
    }

    /// (Re)start a tab's session: spawn a local program, or dial an SSH profile.
    pub(crate) fn start(&mut self, wid: window::Id, id: TabId) -> Task<Message> {
        let grid = self.grids.get(&wid).copied().unwrap_or(DEFAULT_GRID);
        tracing::info!(tab = id.0, "starting tab");
        let fresh_terminal = self.new_terminal(grid);
        let events = self.events.clone();
        let profile: Option<Profile> = self
            .windows
            .get(&wid)
            .and_then(|w| w.find(id).map(|i| w.tabs[i].profile))
            .and_then(|p| self.profiles.get(p).cloned());
        let scheme = self.style.scheme;
        let Some(tab) = self.windows.get_mut(&wid).and_then(|w| {
            let i = w.find(id)?;
            w.tabs.get_mut(i)
        }) else {
            return Task::none();
        };
        // A restart continues in the tab's terminal, so its output and scrollback stay; the
        // first start gets a new one.
        let previous = tab.terminal().cloned();
        // Close the previous session (and drop any dial in flight) before anything touches
        // the terminal again: once closed, its lingering output is discarded, never fed.
        if let Some(old) = tab.session.take() {
            old.close();
        }
        tab.connecting = None;
        let (terminal, fresh) = match previous {
            Some(t) => {
                t.lock().feed(tabs::RESTART_RESET);
                (t, false)
            }
            None => (fresh_terminal, true),
        };
        tab.title = tab.name.clone();
        tab.placeholder = Some(terminal.clone());
        let Some(profile) = profile else {
            tab.state = TabState::Disconnected("profile not found".into());
            write_notice(tab, "this profile no longer exists", fresh);
            return Task::none();
        };
        match &profile.kind {
            ProfileKind::Local(local) => {
                match spawn_local_into(local, terminal, events) {
                    Ok(handle) => {
                        if fresh {
                            scheme.apply_to(&mut handle.terminal.lock());
                        }
                        tab.session = Some(handle);
                        tab.placeholder = None;
                        tab.state = TabState::Connected;
                    }
                    Err(e) => {
                        let text = format!(
                            "could not start {}: {e}, press Enter to retry",
                            profile.name
                        );
                        tab.state = TabState::Disconnected(e.to_string());
                        write_notice(tab, &text, fresh);
                    }
                }
                Task::none()
            }
            ProfileKind::Ssh(ssh) => {
                tab.state = TabState::Connecting;
                write_notice(
                    tab,
                    &format!("connecting to {}@{}:{}", ssh.user, ssh.host, ssh.port),
                    fresh,
                );
                let pool = self.pool.clone();
                let store = self.profiles.clone();
                let pid = profile.id;
                let requested = terminal.lock().size();
                let terminal = terminal.clone();
                // Questions of this dial open over this tab's window.
                let prompter = Arc::new(UiPrompter::new(Some(wid), self.prompt_tx.clone()));
                let dial = async move {
                    pool.open_shell_prompting(&store, &pid, terminal, events, prompter)
                        .await
                };
                let (task, handle) = Task::perform(dial, move |r| match r {
                    Ok(h) => Message::Connected(id, Delivered::new(h), requested),
                    Err(SshError::HostKeyChanged { host, port }) => {
                        let reason = SshError::HostKeyChanged {
                            host: host.clone(),
                            port,
                        }
                        .to_string();
                        Message::HostKeyChanged {
                            tab: id,
                            host,
                            port,
                            reason,
                        }
                    }
                    Err(e) => Message::ConnectFailed(id, e.to_string()),
                })
                .abortable();
                tab.connecting = Some(handle.abort_on_drop());
                task
            }
        }
    }

    /// `requested`: the size the remote pty was opened with. The window may have resized the
    /// terminal while the dial was in flight; the remote side then gets the current size.
    pub(crate) fn connected(&mut self, id: TabId, handle: SessionHandle, requested: GridSize) {
        // A tab closed meanwhile drops the handle here, closing the shell. The session writes
        // into the tab's own terminal, which already carries the color scheme.
        let Some(tab) = self.tab_mut(id) else { return };
        let current = handle.terminal.lock().size();
        if current != requested {
            handle.resize(current);
        }
        tab.session = Some(handle);
        tab.placeholder = None;
        tab.connecting = None;
        tab.state = TabState::Connected;
    }

    pub(crate) fn session_event(&mut self, sid: SessionId, event: SessionEvent) -> Task<Message> {
        let Some((window, index)) = self.windows.values_mut().find_map(|w| {
            let i = w
                .tabs
                .iter()
                .position(|t| t.session.as_ref().is_some_and(|s| s.id == sid))?;
            Some((w, i))
        }) else {
            return Task::none();
        };
        if event == SessionEvent::Dirty {
            // A visible tab re-arms here, before the redraw that follows this update locks
            // the terminal. A hidden tab leaves the flag pending, so its session raises no
            // further Dirty until the tab is shown (see `App::shown`).
            tabs::on_dirty(window, index);
            return Task::none();
        }
        let tab = &mut window.tabs[index];
        match &event {
            SessionEvent::Dirty => Task::none(),
            SessionEvent::ClipboardStore(text) => iced::clipboard::write(text.clone()),
            _ => {
                if let Some(text) = tabs::apply_event(tab, &event) {
                    write_notice(tab, &text, false);
                }
                Task::none()
            }
        }
    }

    pub(crate) fn terminal_output(
        &mut self,
        wid: window::Id,
        output: TerminalOutput,
    ) -> Task<Message> {
        match output {
            TerminalOutput::Input(bytes) => {
                let Some(tab) = self
                    .windows
                    .get_mut(&wid)
                    .and_then(WindowState::active_tab_mut)
                else {
                    return Task::none();
                };
                match tabs::route_input(&tab.state, &bytes) {
                    InputRoute::Send => {
                        if let Some(s) = &tab.session {
                            s.send(bytes);
                        }
                        Task::none()
                    }
                    InputRoute::Reconnect | InputRoute::Respawn => {
                        let id = tab.id;
                        self.start(wid, id)
                    }
                    InputRoute::Ignore => Task::none(),
                }
            }
            TerminalOutput::Resized(grid) => {
                self.grids.insert(wid, grid);
                if let Some(s) = self
                    .windows
                    .get(&wid)
                    .and_then(WindowState::active_tab)
                    .and_then(|t| t.session.as_ref())
                {
                    s.resize(grid);
                }
                Task::none()
            }
            // The widget already put the text on the clipboard.
            TerminalOutput::Copy(_) => Task::none(),
            TerminalOutput::OpenUrl(url) => {
                open_url(&url);
                Task::none()
            }
        }
    }
}

/// Write a dim notice into the tab's visible terminal, keeping its scrollback.
/// `fresh`: the terminal was just created and is still empty.
pub(crate) fn write_notice(tab: &Tab, text: &str, fresh: bool) {
    if let Some(term) = tab.terminal() {
        term.lock().feed(&tabs::notice_bytes(text, fresh));
    }
}

/// What to do with a link the user clicked. The link comes from program output, so only
/// schemes whose handlers never run local programs are opened.
#[derive(Debug, PartialEq, Eq)]
enum LinkAction {
    /// Hand the link to the system opener.
    Open,
    /// A local file: show it in the file manager, never open (and so never run) it.
    Reveal(std::path::PathBuf),
    Refuse,
}

/// Decide what a click on `url` does: web and mail links open; `file://` links to an
/// absolute path are revealed; everything else (`javascript:`, `data:`, `ssh:`, custom app
/// schemes, malformed file links) is refused.
fn link_action(url: &str) -> LinkAction {
    let lower = url.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return LinkAction::Open;
    }
    if lower.starts_with("file://") {
        // file://host/path: the authority (often the host name of the machine that printed
        // the link) is ignored; only the path is used.
        let rest = &url["file://".len()..];
        let path = rest.find('/').map_or("", |i| &rest[i..]);
        let path = path.split(['?', '#']).next().unwrap_or_default();
        return match percent_decode(path) {
            Some(p) if p.starts_with('/') && !p.contains('\0') => {
                LinkAction::Reveal(std::path::PathBuf::from(p))
            }
            _ => LinkAction::Refuse,
        };
    }
    LinkAction::Refuse
}

/// Decode `%XX` escapes; `None` for a malformed escape or a result that is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Act on a clicked link (see [`link_action`]).
fn open_url(url: &str) {
    let cmd = match link_action(url) {
        LinkAction::Open => opener(url),
        LinkAction::Reveal(path) => match reveal(&path) {
            Some(cmd) => cmd,
            None => {
                tracing::info!(path = %path.display(), "not opening a file link on this system");
                return;
            }
        },
        LinkAction::Refuse => {
            tracing::warn!(%url, "not opening link with an unsupported scheme");
            return;
        }
    };
    spawn_reaped(cmd, url);
}

/// The system opener for a web or mail link.
fn opener(url: &str) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url);
    cmd
}

/// Show a local file in the file manager without opening it. Only macOS has a reveal that
/// cannot launch the file; elsewhere file links are logged and left alone.
#[cfg(target_os = "macos")]
fn reveal(path: &std::path::Path) -> Option<std::process::Command> {
    let mut cmd = std::process::Command::new("open");
    // `path` is absolute, so it can never be taken for an option.
    cmd.arg("-R").arg(path);
    Some(cmd)
}

#[cfg(not(target_os = "macos"))]
fn reveal(_path: &std::path::Path) -> Option<std::process::Command> {
    None
}

fn spawn_reaped(mut cmd: std::process::Command, url: &str) {
    match cmd.spawn() {
        // Reap the opener so it does not linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => tracing::warn!(%url, error = %e, "could not open link"),
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkAction, link_action};

    #[test]
    fn web_and_mail_links_open() {
        for url in [
            "http://example.com",
            "https://example.com/a?b#c",
            "HTTPS://EXAMPLE.COM",
            "mailto:someone@example.com",
        ] {
            assert_eq!(link_action(url), LinkAction::Open, "{url}");
        }
    }

    #[test]
    fn file_links_are_revealed_never_opened() {
        assert_eq!(
            link_action("file:///Applications/Calculator.app"),
            LinkAction::Reveal("/Applications/Calculator.app".into())
        );
        assert_eq!(
            link_action("file://build-host/home/me/My%20File.txt"),
            LinkAction::Reveal("/home/me/My File.txt".into())
        );
        assert_eq!(
            link_action("FILE://localhost/tmp/x?q#frag"),
            LinkAction::Reveal("/tmp/x".into())
        );
    }

    #[test]
    fn other_schemes_and_bad_file_links_are_refused() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "ssh://host",
            "vscode://file/x",
            "x-custom-app:run",
            "ftp://example.com",
            "/etc/passwd",
            "",
            "file://",
            "file://hostonly",
            "file:///bad%zz",
            "file:///a%00b",
        ] {
            assert_eq!(link_action(url), LinkAction::Refuse, "{url}");
        }
    }
}
