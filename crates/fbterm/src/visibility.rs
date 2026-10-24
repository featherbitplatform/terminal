//! Whether a terminal window can be seen at all: not minimized, not fully covered, not on a
//! locked screen or a sleeping display. A window that cannot be seen takes no output-driven
//! redraws; its active tab leaves `Dirty` pending (as a hidden tab does) until the window can
//! be seen again.
//!
//! iced 0.14 reports no occlusion events, so on macOS each terminal window observes AppKit's
//! occlusion-state notification itself. Elsewhere every window counts as visible.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use iced::futures::Stream;
use iced::{Task, window};
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// A window became visible (`true`) or stopped being visible (`false`).
pub(crate) type Change = (window::Id, bool);

/// Visibility changes from every window, delivered to the update loop by one subscription.
#[derive(Clone)]
pub(crate) struct Changes {
    tx: UnboundedSender<Change>,
    rx: Arc<Mutex<Option<UnboundedReceiver<Change>>>>,
}

impl Changes {
    pub(crate) fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            tx,
            rx: Arc::new(Mutex::new(Some(rx))),
        }
    }
}

impl Hash for Changes {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "fbterm-window-visibility".hash(state);
    }
}

pub(crate) fn stream(changes: &Changes) -> impl Stream<Item = Change> + use<> {
    let rx = changes.rx.lock().take();
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let change = rx.recv().await?;
        Some((change, Some(rx)))
    })
}

/// Start reporting the visibility of window `id`, which has just opened.
pub(crate) fn watch<M: Send + 'static>(id: window::Id, changes: &Changes) -> Task<M> {
    #[cfg(target_os = "macos")]
    {
        let tx = changes.tx.clone();
        window::run(id, move |w| mac::watch(id, w, tx)).discard()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (id, &changes.tx);
        Task::none()
    }
}

/// Stop reporting the visibility of window `id`, which has closed.
pub(crate) fn unwatch(id: window::Id) {
    #[cfg(target_os = "macos")]
    mac::unwatch(id);
    #[cfg(not(target_os = "macos"))]
    let _ = id;
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use iced::window::{self, raw_window_handle::RawWindowHandle};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{
        NSView, NSWindow, NSWindowDidChangeOcclusionStateNotification, NSWindowOcclusionState,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    use tokio::sync::mpsc::UnboundedSender;

    use super::Change;

    type Observer = Retained<ProtocolObject<dyn NSObjectProtocol>>;

    thread_local! {
        /// The notification observer of each watched window (main thread only).
        static OBSERVERS: RefCell<HashMap<window::Id, Observer>> = RefCell::new(HashMap::new());
    }

    fn any(o: &Observer) -> &AnyObject {
        o.as_ref()
    }

    fn visible(w: &NSWindow) -> bool {
        w.occlusionState().contains(NSWindowOcclusionState::Visible)
    }

    pub(super) fn watch(id: window::Id, w: &dyn window::Window, tx: UnboundedSender<Change>) {
        // Window callbacks run on the main thread, as AppKit requires.
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Ok(handle) = w.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        // SAFETY: the handle points at the window's live content view for the duration of
        // this callback, and we are on the main thread.
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        let Some(ns_window) = view.window() else {
            return;
        };
        let report = tx.clone();
        let block = RcBlock::new(move |n: NonNull<NSNotification>| {
            // SAFETY: AppKit passes a valid notification for the duration of the call.
            let n = unsafe { n.as_ref() };
            if let Some(w) = n.object().and_then(|o| o.downcast::<NSWindow>().ok()) {
                let _ = report.send((id, visible(&w)));
            }
        });
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: the observed object is an NSWindow, matching the notification; with no
        // queue the block runs on the posting (main) thread, where it is only ever used.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWindowDidChangeOcclusionStateNotification),
                Some(&ns_window),
                None,
                &block,
            )
        };
        if let Some(old) = OBSERVERS.with(|o| o.borrow_mut().insert(id, observer)) {
            // SAFETY: `old` was returned by addObserverForName on this center.
            unsafe { center.removeObserver(any(&old)) };
        }
        let _ = tx.send((id, visible(&ns_window)));
    }

    pub(super) fn unwatch(id: window::Id) {
        if let Some(observer) = OBSERVERS.with(|o| o.borrow_mut().remove(&id)) {
            debug_assert!(MainThreadMarker::new().is_some());
            // Safe after the NSWindow is gone: the center neither retains nor dereferences the
            // `object:` filter, only the observer token is used.
            // SAFETY: `observer` was returned by addObserverForName on the default center.
            unsafe { NSNotificationCenter::defaultCenter().removeObserver(any(&observer)) };
        }
    }
}
