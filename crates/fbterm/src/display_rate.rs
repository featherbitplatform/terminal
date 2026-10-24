//! The refresh rate of the display each terminal window is on, for `max_fps = "display"`.
//!
//! iced 0.14 exposes no monitor refresh rate (a window callback only gets the raw handles), so
//! on macOS each terminal window asks AppKit for its screen's `maximumFramesPerSecond` when it
//! opens, when it moves to another screen (`NSWindowDidChangeScreenNotification`) and when it
//! gains focus. Elsewhere the rate is unknown and `"display"` uses the default rate.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use iced::futures::Stream;
use iced::{Task, window};
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Window `id` is on a display refreshing this many times per second (`None`: unknown).
pub(crate) type Change = (window::Id, Option<u32>);

/// Refresh-rate changes from every window, delivered to the update loop by one subscription.
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
        "fbterm-display-rate".hash(state);
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

/// Start reporting the display rate of window `id`, which has just opened.
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

/// Report the display rate of window `id` again (cheap; on focus). A window whose AppKit
/// window was not there yet when it opened starts being watched here.
pub(crate) fn detect<M: Send + 'static>(id: window::Id, changes: &Changes) -> Task<M> {
    #[cfg(target_os = "macos")]
    {
        let tx = changes.tx.clone();
        window::run(id, move |w| mac::detect(id, w, tx)).discard()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (id, &changes.tx);
        Task::none()
    }
}

/// Stop reporting the display rate of window `id`, which has closed.
pub(crate) fn unwatch(id: window::Id) {
    #[cfg(target_os = "macos")]
    mac::unwatch(id);
    #[cfg(not(target_os = "macos"))]
    let _ = id;
}

/// The rate to keep for a window after a report of `reported`: an unknown rate (a window
/// briefly without a screen) never replaces a known one.
pub(crate) fn merge(known: Option<Option<u32>>, reported: Option<u32>) -> Option<u32> {
    reported.or(known.flatten())
}

/// The rate `"display"` follows: the fastest display any terminal window is on, if known.
/// Pacing is shared by every window, so the fastest one sets it.
pub(crate) fn fastest(rates: impl IntoIterator<Item = Option<u32>>) -> Option<u32> {
    rates.into_iter().flatten().filter(|hz| *hz > 0).max()
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
    use objc2_app_kit::{NSView, NSWindow, NSWindowDidChangeScreenNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    use tokio::sync::mpsc::UnboundedSender;

    use super::Change;

    type Observer = Retained<ProtocolObject<dyn NSObjectProtocol>>;

    thread_local! {
        /// The screen-change observer of each watched window (main thread only).
        static OBSERVERS: RefCell<HashMap<window::Id, Observer>> = RefCell::new(HashMap::new());
    }

    fn any(o: &Observer) -> &AnyObject {
        o.as_ref()
    }

    /// The highest refresh rate of the screen `w` is on (variable-rate screens report their
    /// maximum), or `None` off screen.
    pub(super) fn rate(w: &NSWindow) -> Option<u32> {
        let fps = w.screen()?.maximumFramesPerSecond();
        u32::try_from(fps).ok().filter(|hz| *hz > 0)
    }

    /// The AppKit window behind `w`; `None` off the main thread or for a non-AppKit handle.
    pub(super) fn ns_window(w: &dyn window::Window) -> Option<Retained<NSWindow>> {
        // Window callbacks run on the main thread, as AppKit requires.
        MainThreadMarker::new()?;
        let handle = w.window_handle().ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: the handle points at the window's live content view for the duration of
        // this callback, and we are on the main thread.
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        view.window()
    }

    pub(super) fn watch(id: window::Id, w: &dyn window::Window, tx: UnboundedSender<Change>) {
        let Some(ns_window) = ns_window(w) else {
            return;
        };
        let report = tx.clone();
        let block = RcBlock::new(move |n: NonNull<NSNotification>| {
            // SAFETY: AppKit passes a valid notification for the duration of the call.
            let n = unsafe { n.as_ref() };
            if let Some(w) = n.object().and_then(|o| o.downcast::<NSWindow>().ok()) {
                let _ = report.send((id, rate(&w)));
            }
        });
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: the observed object is an NSWindow, matching the notification; with no
        // queue the block runs on the posting (main) thread, where it is only ever used.
        let observer = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWindowDidChangeScreenNotification),
                Some(&ns_window),
                None,
                &block,
            )
        };
        if let Some(old) = OBSERVERS.with(|o| o.borrow_mut().insert(id, observer)) {
            // SAFETY: `old` was returned by addObserverForName on this center.
            unsafe { center.removeObserver(any(&old)) };
        }
        let _ = tx.send((id, rate(&ns_window)));
    }

    /// Report the rate of `w` again, and start watching it if [`watch`] could not (its
    /// AppKit window did not exist yet when it opened).
    pub(super) fn detect(id: window::Id, w: &dyn window::Window, tx: UnboundedSender<Change>) {
        if !OBSERVERS.with(|o| o.borrow().contains_key(&id)) {
            watch(id, w, tx);
            return;
        }
        if let Some(ns_window) = ns_window(w) {
            let _ = tx.send((id, rate(&ns_window)));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_rate_never_replaces_a_known_one() {
        assert_eq!(merge(Some(Some(120)), None), Some(120));
        assert_eq!(merge(Some(Some(120)), Some(60)), Some(60));
        assert_eq!(merge(None, None), None);
        assert_eq!(merge(Some(None), Some(60)), Some(60));
    }

    #[test]
    fn the_fastest_known_display_wins() {
        assert_eq!(fastest([Some(60), Some(120), None]), Some(120));
        assert_eq!(fastest([None, Some(0)]), None);
        assert_eq!(fastest([]), None);
    }
}
