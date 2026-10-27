//! Connection prompts: the [`Prompter`] that asks through the UI, and the per-window queue of
//! dialogs it feeds (trust this host?, changed host key, password or passphrase), and the app's
//! own questions in the same queue (closing a split tab, an upload's name clash, closing while
//! transfers run).
//!
//! A dial asks from its own task: [`UiPrompter`] sends the question with a `oneshot` reply
//! channel to the update loop and waits. Dropping the reply (window closed, dialog dismissed)
//! answers "no" / "cancelled". A question whose dial is given up is withdrawn from the queue.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fbterm_secrets::SecretKind;
use fbterm_session::ssh::tunnel::TunnelKey;
use fbterm_session::ssh::{Prompter, SecretAnswer};
use iced::futures::Stream;
use iced::window;
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::app::Delivered;
use crate::layout::PaneId;
use crate::redact::Redacted;
use crate::tabs::TabId;

/// Identifies one question for the lifetime of the process.
pub type PromptId = u64;

fn next_id() -> PromptId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A question from a dial, with the channel its answer goes back on.
pub enum Ask {
    HostKey {
        host: String,
        port: u16,
        fingerprint: String,
        reply: oneshot::Sender<bool>,
    },
    Secret {
        profile: String,
        kind: SecretKind,
        retry: bool,
        reply: oneshot::Sender<Option<SecretAnswer>>,
    },
}

/// What a [`UiPrompter`] tells the update loop.
pub enum PromptEvent {
    /// Show `ask` over `window` (`None`: the focused window).
    Ask {
        id: PromptId,
        window: Option<window::Id>,
        /// The tunnel asking, when a tunnel's connect asks: its window is raised, and with no
        /// terminal window to ask in the tunnel fails.
        tunnel: Option<TunnelKey>,
        ask: Ask,
    },
    /// The asker stopped waiting (answered, or its dial was given up).
    Withdraw(PromptId),
}

/// Asks the user through the UI, over the window the dial belongs to.
pub struct UiPrompter {
    window: Option<window::Id>,
    tunnel: Option<TunnelKey>,
    tx: UnboundedSender<PromptEvent>,
}

impl UiPrompter {
    pub fn new(window: Option<window::Id>, tx: UnboundedSender<PromptEvent>) -> Self {
        Self {
            window,
            tunnel: None,
            tx,
        }
    }

    /// The prompter of tunnel `key`'s connect (see [`PromptEvent::Ask`]).
    pub fn for_tunnel(
        window: Option<window::Id>,
        key: TunnelKey,
        tx: UnboundedSender<PromptEvent>,
    ) -> Self {
        Self {
            window,
            tunnel: Some(key),
            tx,
        }
    }

    /// Send `ask` and wait for the answer; `None` if the UI dropped the question.
    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Ask) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        let id = next_id();
        let event = PromptEvent::Ask {
            id,
            window: self.window,
            tunnel: self.tunnel.clone(),
            ask: make(reply),
        };
        if self.tx.send(event).is_err() {
            return None;
        }
        // Withdraws the dialog once this question is over, also when the dial is dropped
        // while waiting.
        let _withdraw = Withdraw {
            id,
            tx: self.tx.clone(),
        };
        answer.await.ok()
    }
}

struct Withdraw {
    id: PromptId,
    tx: UnboundedSender<PromptEvent>,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        let _ = self.tx.send(PromptEvent::Withdraw(self.id));
    }
}

#[async_trait::async_trait]
impl Prompter for UiPrompter {
    async fn confirm_host_key(&self, host: &str, port: u16, fingerprint: &str) -> bool {
        self.ask(|reply| Ask::HostKey {
            host: host.to_owned(),
            port,
            fingerprint: fingerprint.to_owned(),
            reply,
        })
        .await
        .unwrap_or(false)
    }

    async fn ask_secret(
        &self,
        profile: &str,
        kind: SecretKind,
        retry: bool,
    ) -> Option<SecretAnswer> {
        self.ask(|reply| Ask::Secret {
            profile: profile.to_owned(),
            kind,
            retry,
            reply,
        })
        .await
        .flatten()
    }
}

/// Prompt events, delivered to the update loop by one subscription.
#[derive(Clone)]
pub struct PromptReceiver(pub Arc<Mutex<Option<UnboundedReceiver<PromptEvent>>>>);

impl Hash for PromptReceiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "fbterm-prompt-events".hash(state);
    }
}

pub fn prompt_stream(rx: &PromptReceiver) -> impl Stream<Item = PromptMsg> + use<> {
    let rx = rx.0.lock().take();
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let event = rx.recv().await?;
        Some((PromptMsg::Event(Delivered::new(event)), Some(rx)))
    })
}

/// What the prompt overlay asks of the app.
#[derive(Debug, Clone)]
pub enum PromptMsg {
    Event(Delivered<PromptEvent>),
    /// Trust the host key shown in this window.
    Trust(window::Id),
    /// Cancel (or close) this window's dialog.
    Cancel(window::Id),
    Input(window::Id, Redacted),
    Remember(window::Id, bool),
    /// Enter: submit a secret (ignored by the other dialogs).
    Submit(window::Id),
    Copy(String),
    /// Close the tab the shown close confirmation names.
    CloseTab(window::Id),
    /// The clash dialog's `Replace`.
    ClashReplace(window::Id),
    /// The clash dialog's `Rename`: show the name field.
    ClashRename(window::Id),
    /// The clash dialog's `Apply to all`.
    ClashApplyAll(window::Id, bool),
    /// Typing in the clash dialog's name field.
    ClashName(window::Id, String),
    /// `Cancel transfers and close` in the close confirmation.
    StopTransfers(window::Id),
}

/// An upload whose name exists in its folder: `"{name}" already exists in {folder}.`
pub struct Clash {
    /// The name as the server would get it (shown as plain text).
    pub name: String,
    pub folder: String,
    /// `Apply to all` is offered: other files of the same upload are still to decide.
    pub offer_all: bool,
    pub apply_all: bool,
    /// The `Rename` field, once asked for.
    pub rename: Option<Rename>,
}

/// The name field of a clash dialog.
pub struct Rename {
    pub value: String,
    /// Why the typed name cannot be used.
    pub problem: Option<String>,
    pub input: iced::widget::Id,
}

/// What closes once the user agrees to cancel the transfers it would end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    Tab(TabId),
    Pane(PaneId),
    /// The window the question is shown in.
    Window,
    Quit,
}

/// A dialog waiting in a window's queue.
pub enum Dialog {
    HostKey {
        host: String,
        port: u16,
        fingerprint: String,
        reply: Option<oneshot::Sender<bool>>,
    },
    /// A refused, changed host key: explained, never accepted.
    Changed { host: String, port: u16 },
    /// Closing a whole tab that has `panes` panes, more than one of them live: asked first.
    CloseTab { tab: TabId, panes: usize },
    /// An upload's name exists in its folder: `Replace`, `Rename` or `Cancel`.
    Clash(Clash),
    /// Closing (or quitting) would end `count` running transfers: asked first.
    Transfers { count: usize, then: Closing },
    Secret {
        profile: String,
        kind: SecretKind,
        retry: bool,
        value: String,
        remember: bool,
        reply: Option<oneshot::Sender<Option<SecretAnswer>>>,
        input: iced::widget::Id,
    },
}

impl Dialog {
    fn from_ask(ask: Ask) -> Self {
        match ask {
            Ask::HostKey {
                host,
                port,
                fingerprint,
                reply,
            } => Self::HostKey {
                host,
                port,
                fingerprint,
                reply: Some(reply),
            },
            Ask::Secret {
                profile,
                kind,
                retry,
                reply,
            } => Self::Secret {
                profile,
                kind,
                retry,
                value: String::new(),
                remember: true,
                reply: Some(reply),
                input: iced::widget::Id::unique(),
            },
        }
    }

    /// Whether Enter / Connect answers: a secret prompt needs something typed.
    pub fn can_submit(&self) -> bool {
        matches!(self, Self::Secret { value, .. } if !value.is_empty())
    }

    /// The field to focus when this dialog is shown.
    pub fn input(&self) -> Option<iced::widget::Id> {
        match self {
            Self::Secret { input, .. } => Some(input.clone()),
            Self::Clash(Clash {
                rename: Some(r), ..
            }) => Some(r.input.clone()),
            _ => None,
        }
    }

    /// Answer "no": refuse the key, or cancel the secret.
    fn reject(self) {
        match self {
            Self::HostKey { reply, .. } => {
                if let Some(r) = reply {
                    let _ = r.send(false);
                }
            }
            Self::Secret { reply, .. } => {
                if let Some(r) = reply {
                    let _ = r.send(None);
                }
            }
            Self::Changed { .. }
            | Self::CloseTab { .. }
            | Self::Clash(_)
            | Self::Transfers { .. } => {}
        }
    }
}

pub struct Pending {
    /// `None` for dialogs raised by the app itself (changed key).
    id: Option<PromptId>,
    pub dialog: Dialog,
}

/// Dialogs per window, shown one at a time, oldest first.
#[derive(Default)]
pub struct PromptQueues(HashMap<window::Id, VecDeque<Pending>>);

impl PromptQueues {
    /// Queue a dialog from an asker; returns whether it is now the one shown.
    pub fn ask(&mut self, window: window::Id, id: PromptId, ask: Ask) -> bool {
        self.push(window, Some(id), Dialog::from_ask(ask))
    }

    /// Queue an app dialog (changed key); returns whether it is now the one shown.
    pub fn push(&mut self, window: window::Id, id: Option<PromptId>, dialog: Dialog) -> bool {
        let q = self.0.entry(window).or_default();
        q.push_back(Pending { id, dialog });
        q.len() == 1
    }

    pub fn front(&self, window: window::Id) -> Option<&Dialog> {
        self.0.get(&window)?.front().map(|p| &p.dialog)
    }

    pub fn front_mut(&mut self, window: window::Id) -> Option<&mut Dialog> {
        self.0.get_mut(&window)?.front_mut().map(|p| &mut p.dialog)
    }

    /// Take the shown dialog off its window's queue.
    pub fn pop(&mut self, window: window::Id) -> Option<Dialog> {
        let q = self.0.get_mut(&window)?;
        let p = q.pop_front();
        if q.is_empty() {
            self.0.remove(&window);
        }
        p.map(|p| p.dialog)
    }

    /// Remove a question wherever it is; returns the window if it was the one shown there.
    pub fn withdraw(&mut self, id: PromptId) -> Option<window::Id> {
        let (&window, q) = self
            .0
            .iter_mut()
            .find(|(_, q)| q.iter().any(|p| p.id == Some(id)))?;
        let pos = q.iter().position(|p| p.id == Some(id))?;
        q.remove(pos);
        if q.is_empty() {
            self.0.remove(&window);
        }
        (pos == 0).then_some(window)
    }

    /// Drop a window's dialogs (it closed): every asker gets "no".
    pub fn remove_window(&mut self, window: window::Id) {
        if let Some(q) = self.0.remove(&window) {
            for p in q {
                p.dialog.reject();
            }
        }
    }

    /// Cancel the shown dialog.
    pub fn cancel(&mut self, window: window::Id) {
        if let Some(d) = self.pop(window) {
            d.reject();
        }
    }

    /// Trust the shown host key. Other dialogs are left alone.
    pub fn trust(&mut self, window: window::Id) {
        if matches!(self.front(window), Some(Dialog::HostKey { .. }))
            && let Some(Dialog::HostKey { reply: Some(r), .. }) = self.pop(window)
        {
            let _ = r.send(true);
        }
    }

    /// Submit the shown secret prompt, if something was typed. Other dialogs are left alone.
    pub fn submit(&mut self, window: window::Id) {
        if self.front(window).is_some_and(Dialog::can_submit)
            && let Some(Dialog::Secret {
                value,
                remember,
                reply: Some(r),
                ..
            }) = self.pop(window)
        {
            let _ = r.send(Some(SecretAnswer { value, remember }));
        }
    }

    /// Take the shown close confirmation off the queue: the tab to close. Other dialogs are
    /// left alone.
    pub fn confirm_close(&mut self, window: window::Id) -> Option<TabId> {
        let Some(Dialog::CloseTab { tab, .. }) = self.front(window) else {
            return None;
        };
        let tab = *tab;
        self.pop(window);
        Some(tab)
    }

    /// Whether `window` already asks about closing `tab`.
    pub fn has_close(&self, window: window::Id, tab: TabId) -> bool {
        self.0.get(&window).is_some_and(|q| {
            q.iter()
                .any(|p| matches!(&p.dialog, Dialog::CloseTab { tab: t, .. } if *t == tab))
        })
    }

    /// Whether `window` already asks about closing with transfers running for `then`.
    pub fn has_transfers(&self, window: window::Id, then: Closing) -> bool {
        self.0.get(&window).is_some_and(|q| {
            q.iter()
                .any(|p| matches!(&p.dialog, Dialog::Transfers { then: t, .. } if *t == then))
        })
    }

    /// Every close confirmation queued: its window and what it would close.
    pub fn transfer_questions(&self) -> Vec<(window::Id, Closing)> {
        self.0
            .iter()
            .flat_map(|(w, q)| {
                q.iter().filter_map(move |p| match &p.dialog {
                    Dialog::Transfers { then, .. } => Some((*w, *then)),
                    _ => None,
                })
            })
            .collect()
    }

    /// Set the count of `window`'s close confirmation for `then`; at 0 there is nothing left
    /// to ask about, and it goes.
    pub fn set_transfers(&mut self, window: window::Id, then: Closing, count: usize) {
        let Some(q) = self.0.get_mut(&window) else {
            return;
        };
        if count == 0 {
            q.retain(|p| !matches!(&p.dialog, Dialog::Transfers { then: t, .. } if *t == then));
            if q.is_empty() {
                self.0.remove(&window);
            }
            return;
        }
        for p in q.iter_mut() {
            if let Dialog::Transfers { count: c, then: t } = &mut p.dialog
                && *t == then
            {
                *c = count;
            }
        }
    }

    /// Whether any window asks about quitting with transfers running.
    pub fn asks_quit(&self) -> bool {
        self.0.values().flatten().any(|p| {
            matches!(
                &p.dialog,
                Dialog::Transfers {
                    then: Closing::Quit,
                    ..
                }
            )
        })
    }

    /// Whether `window` already has a changed-key dialog for this host.
    pub fn has_changed(&self, window: window::Id, host: &str, port: u16) -> bool {
        self.0.get(&window).is_some_and(|q| {
            q.iter().any(|p| {
                matches!(&p.dialog, Dialog::Changed { host: h, port: n } if h == host && *n == port)
            })
        })
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The command that removes a host's recorded keys from `known_hosts`.
pub fn keygen_command(host: &str, port: u16) -> String {
    if port == 22 {
        format!("ssh-keygen -R {host}")
    } else {
        format!("ssh-keygen -R \"[{host}]:{port}\"")
    }
}

/// `host:port`, bracketing IPv6 addresses.
pub fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn wid() -> window::Id {
        window::Id::unique()
    }

    /// Receive the next Ask, queue it in `q` over `w`.
    async fn queue(
        rx: &mut UnboundedReceiver<PromptEvent>,
        q: &mut PromptQueues,
        w: window::Id,
    ) -> PromptId {
        match rx.recv().await.unwrap() {
            PromptEvent::Ask { id, ask, .. } => {
                q.ask(w, id, ask);
                id
            }
            PromptEvent::Withdraw(_) => panic!("expected an ask"),
        }
    }

    #[tokio::test]
    async fn host_key_answer_reaches_the_dial() {
        let (tx, mut rx) = unbounded_channel();
        let p = UiPrompter::new(None, tx);
        let w = wid();
        let mut q = PromptQueues::default();
        let (trusted, ()) = tokio::join!(p.confirm_host_key("h", 22, "SHA256:abc"), async {
            queue(&mut rx, &mut q, w).await;
            assert!(matches!(
                q.front(w),
                Some(Dialog::HostKey { fingerprint, .. }) if fingerprint == "SHA256:abc"
            ));
            q.trust(w);
        });
        assert!(trusted);
        assert!(q.is_empty());
    }

    #[tokio::test]
    async fn closing_the_window_rejects_and_cancels() {
        let (tx, mut rx) = unbounded_channel();
        let p = UiPrompter::new(None, tx);
        let w = wid();
        let mut q = PromptQueues::default();
        let (trusted, secret, ()) = tokio::join!(
            p.confirm_host_key("h", 22, "SHA256:abc"),
            p.ask_secret("web", SecretKind::Password, false),
            async {
                queue(&mut rx, &mut q, w).await;
                queue(&mut rx, &mut q, w).await;
                q.remove_window(w);
            }
        );
        assert!(!trusted);
        assert!(secret.is_none());
    }

    #[tokio::test]
    async fn dropped_ui_answers_no() {
        let (tx, rx) = unbounded_channel();
        drop(rx);
        let p = UiPrompter::new(None, tx);
        assert!(!p.confirm_host_key("h", 22, "SHA256:abc").await);
        assert!(
            p.ask_secret("web", SecretKind::Password, true)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn secret_submit_carries_value_and_remember() {
        let (tx, mut rx) = unbounded_channel();
        let p = UiPrompter::new(None, tx);
        let w = wid();
        let mut q = PromptQueues::default();
        let (answer, ()) = tokio::join!(p.ask_secret("web", SecretKind::Password, true), async {
            queue(&mut rx, &mut q, w).await;
            q.trust(w); // not a host key: ignored
            let Some(Dialog::Secret {
                value,
                remember,
                retry,
                ..
            }) = q.front_mut(w)
            else {
                panic!("secret dialog")
            };
            assert!(*retry && *remember, "remember is checked by default");
            value.push_str("pw");
            *remember = false;
            q.submit(w);
        });
        let answer = answer.unwrap();
        assert_eq!((answer.value.as_str(), answer.remember), ("pw", false));
    }

    #[tokio::test]
    async fn given_up_question_is_withdrawn() {
        let (tx, mut rx) = unbounded_channel();
        let p = UiPrompter::new(None, tx);
        let w = wid();
        let mut q = PromptQueues::default();
        {
            let ask = p.confirm_host_key("h", 22, "SHA256:abc");
            tokio::pin!(ask);
            // Poll once so the question is sent, then drop it (the dial was aborted).
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut ask)
                    .await
                    .is_err()
            );
        }
        let id = queue(&mut rx, &mut q, w).await;
        match rx.recv().await.unwrap() {
            PromptEvent::Withdraw(gone) => assert_eq!(gone, id),
            PromptEvent::Ask { .. } => panic!("expected a withdraw"),
        }
        assert_eq!(q.withdraw(id), Some(w));
        assert!(q.is_empty());
    }

    #[tokio::test]
    async fn empty_secret_is_not_submitted() {
        let (tx, mut rx) = unbounded_channel();
        let p = UiPrompter::new(None, tx);
        let w = wid();
        let mut q = PromptQueues::default();
        let (answer, ()) = tokio::join!(p.ask_secret("web", SecretKind::Password, false), async {
            queue(&mut rx, &mut q, w).await;
            q.submit(w);
            assert!(
                matches!(q.front(w), Some(Dialog::Secret { .. })),
                "Enter on an empty field keeps the dialog"
            );
            assert!(!q.front(w).unwrap().can_submit());
            if let Some(Dialog::Secret { value, .. }) = q.front_mut(w) {
                value.push_str("pw");
            }
            assert!(q.front(w).unwrap().can_submit());
            q.submit(w);
        });
        assert_eq!(answer.unwrap().value, "pw");
    }

    #[test]
    fn typed_secret_is_redacted_in_debug() {
        let msg = PromptMsg::Input(wid(), Redacted("hunter2-secret".into()));
        let app = crate::app::Message::Prompt(msg.clone());
        for shown in [format!("{msg:?}"), format!("{app:?}")] {
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(shown.contains("<redacted>"), "{shown}");
        }
    }

    #[test]
    fn queue_shows_one_dialog_per_window_in_order() {
        let mut q = PromptQueues::default();
        let (a, b) = (wid(), wid());
        let changed = |h: &str| Dialog::Changed {
            host: h.into(),
            port: 22,
        };
        assert!(q.push(a, Some(1), changed("one")));
        assert!(!q.push(a, Some(2), changed("two")));
        assert!(q.push(b, Some(3), changed("three")));
        assert!(matches!(q.front(a), Some(Dialog::Changed { host, .. }) if host == "one"));
        assert_eq!(q.withdraw(2), None, "a queued, unseen dialog");
        q.cancel(a);
        assert!(q.front(a).is_none());
        assert!(matches!(q.front(b), Some(Dialog::Changed { host, .. }) if host == "three"));
    }

    #[test]
    fn keygen_command_brackets_non_default_ports() {
        assert_eq!(keygen_command("db.corp", 22), "ssh-keygen -R db.corp");
        assert_eq!(
            keygen_command("127.0.0.1", 2222),
            "ssh-keygen -R \"[127.0.0.1]:2222\""
        );
        assert_eq!(host_port("::1", 22), "[::1]:22");
        assert_eq!(host_port("h", 2222), "h:2222");
    }
}
