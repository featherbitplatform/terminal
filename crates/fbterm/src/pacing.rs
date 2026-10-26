//! Pacing output-driven redraws.
//!
//! Every message the update loop handles redraws every window, so a stream of output would
//! otherwise draw a frame per chunk, as fast as presenting allows (and without limit when
//! presenting is not paced by the display, for example while it sleeps). Session `Dirty`
//! events therefore pass through a [`Pacer`] before they reach the update loop: the first one
//! after a quiet interval goes through at once, later ones wait for the next slot and are
//! delivered together. Input-driven redraws (keys, selection, scrolling, resizing, focus) are
//! not session events and stay immediate; output right after a keypress (its echo) also skips
//! the wait.
//!
//! The rate is the `max_fps` setting (default [`OUTPUT_FPS`]); `"display"` follows the fastest
//! display a terminal window is on (see `display_rate.rs`). The app keeps the rate in force in
//! an [`OutputRate`], which the pacing stream reads for every event, so a change applies at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Default output-driven frames per second, and the rate for `"display"` when no display rate
/// is known. Every message redraws every window, so the rate bounds each window too.
pub const OUTPUT_FPS: u32 = fbterm_config::settings::DEFAULT_MAX_FPS as u32;

/// The output redraw rate in force, shared by the app (which sets it from the settings and the
/// displays) and the pacing stream (which reads it).
#[derive(Clone, Debug)]
pub(crate) struct OutputRate(Arc<AtomicU32>);

impl OutputRate {
    pub(crate) fn new(fps: u32) -> Self {
        Self(Arc::new(AtomicU32::new(fps)))
    }

    pub(crate) fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn set(&self, fps: u32) {
        self.0.store(fps, Ordering::Relaxed);
    }
}

fn interval(fps: u32) -> Duration {
    Duration::from_secs(1) / fps.max(1)
}

/// Decides when pending output is delivered. Pure logic: the caller passes the time.
pub struct Pacer<T> {
    interval: Duration,
    /// When output was last delivered.
    last: Option<Instant>,
    /// Sessions with output waiting, in arrival order.
    pending: Vec<T>,
    /// When the pending output is due, if any is waiting.
    deadline: Option<Instant>,
}

impl<T: PartialEq> Pacer<T> {
    pub fn new(fps: u32) -> Self {
        Self {
            interval: interval(fps),
            last: None,
            pending: Vec::new(),
            deadline: None,
        }
    }

    /// Change the rate. Output already waiting keeps its slot; later slots use the new rate.
    pub fn set_fps(&mut self, fps: u32) {
        self.interval = interval(fps);
    }

    /// The time between deliveries.
    #[cfg(test)]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Session `id` has new output at `now`; `after_input`: the user typed since the last
    /// delivery, so this is likely the echo. Returns the sessions to deliver now (empty while
    /// they wait for [`Pacer::deadline`]).
    pub fn dirty(&mut self, id: T, now: Instant, after_input: bool) -> Vec<T> {
        if !self.pending.contains(&id) {
            self.pending.push(id);
        }
        match self.last {
            Some(last) if !after_input && now < last + self.interval => {
                self.deadline.get_or_insert(last + self.interval);
                Vec::new()
            }
            _ => self.flush(now),
        }
    }

    /// When the waiting output is due; `None` when nothing waits.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The deadline passed: deliver everything waiting.
    pub fn fire(&mut self, now: Instant) -> Vec<T> {
        if self.pending.is_empty() {
            self.deadline = None;
            return Vec::new();
        }
        self.flush(now)
    }

    fn flush(&mut self, now: Instant) -> Vec<T> {
        self.deadline = None;
        self.last = Some(now);
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn first_output_goes_at_once() {
        let mut p = Pacer::new(60);
        assert_eq!(p.dirty(1, Instant::now(), false), vec![1]);
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn output_within_the_interval_waits_for_the_slot() {
        let t0 = Instant::now();
        let mut p = Pacer::new(50); // 20 ms
        assert_eq!(p.dirty(1, t0, false), vec![1]);
        assert!(p.dirty(1, t0 + ms(5), false).is_empty());
        assert_eq!(p.deadline(), Some(t0 + ms(20)));
        // More output before the slot neither moves it nor duplicates the session.
        assert!(p.dirty(2, t0 + ms(10), false).is_empty());
        assert!(p.dirty(1, t0 + ms(12), false).is_empty());
        assert_eq!(p.deadline(), Some(t0 + ms(20)));
        assert_eq!(p.fire(t0 + ms(20)), vec![1, 2]);
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn output_after_a_quiet_interval_goes_at_once() {
        let t0 = Instant::now();
        let mut p = Pacer::new(50);
        p.dirty(1, t0, false);
        assert_eq!(p.dirty(1, t0 + ms(20), false), vec![1]);
        assert_eq!(p.dirty(1, t0 + ms(500), false), vec![1]);
    }

    #[test]
    fn echo_after_input_skips_the_wait() {
        let t0 = Instant::now();
        let mut p = Pacer::new(50);
        p.dirty(1, t0, false);
        assert!(p.dirty(1, t0 + ms(2), false).is_empty());
        // A keypress: the pending output and the echo go out now, not at the slot.
        assert_eq!(p.dirty(1, t0 + ms(4), true), vec![1]);
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn a_stream_is_capped_at_the_rate() {
        // Output every 10 ms for one second (100 chunks/s) at a 60/s cap.
        let t0 = Instant::now();
        let mut p = Pacer::new(OUTPUT_FPS);
        let mut delivered = 0;
        let mut now = t0;
        while now < t0 + Duration::from_secs(1) {
            if let Some(d) = p.deadline()
                && d <= now
            {
                delivered += usize::from(!p.fire(d).is_empty());
            }
            delivered += usize::from(!p.dirty(1, now, false).is_empty());
            now += ms(10);
        }
        assert!((50..=61).contains(&delivered), "{delivered} deliveries");
    }

    #[test]
    fn the_interval_follows_the_cap() {
        let mut p: Pacer<u32> = Pacer::new(OUTPUT_FPS);
        assert_eq!(p.interval(), Duration::from_nanos(16_666_666));
        p.set_fps(120);
        assert_eq!(p.interval(), Duration::from_nanos(8_333_333));
        p.set_fps(30);
        assert_eq!(p.interval(), Duration::from_nanos(33_333_333));
        // Never a zero rate.
        p.set_fps(0);
        assert_eq!(p.interval(), Duration::from_secs(1));
    }

    #[test]
    fn a_rate_change_applies_from_the_next_slot() {
        let t0 = Instant::now();
        let mut p = Pacer::new(50); // 20 ms
        p.dirty(1, t0, false);
        assert!(p.dirty(1, t0 + ms(5), false).is_empty());
        p.set_fps(100); // 10 ms
        // The waiting output keeps its slot.
        assert_eq!(p.deadline(), Some(t0 + ms(20)));
        assert_eq!(p.fire(t0 + ms(20)), vec![1]);
        assert!(p.dirty(1, t0 + ms(25), false).is_empty());
        assert_eq!(p.deadline(), Some(t0 + ms(30)));
    }

    #[test]
    fn a_stream_is_capped_at_a_higher_rate() {
        // Output every 2 ms for one second (500 chunks/s) at a 120/s cap.
        let t0 = Instant::now();
        let mut p = Pacer::new(120);
        let mut delivered = 0;
        let mut now = t0;
        while now < t0 + Duration::from_secs(1) {
            if let Some(d) = p.deadline()
                && d <= now
            {
                delivered += usize::from(!p.fire(d).is_empty());
            }
            delivered += usize::from(!p.dirty(1, now, false).is_empty());
            now += ms(2);
        }
        assert!((100..=121).contains(&delivered), "{delivered} deliveries");
    }

    #[test]
    fn the_shared_rate_is_read_live() {
        let rate = OutputRate::new(OUTPUT_FPS);
        let other = rate.clone();
        other.set(144);
        assert_eq!(rate.get(), 144);
    }

    #[test]
    fn fire_with_nothing_waiting_delivers_nothing() {
        let mut p: Pacer<u32> = Pacer::new(60);
        assert!(p.fire(Instant::now()).is_empty());
    }
}
