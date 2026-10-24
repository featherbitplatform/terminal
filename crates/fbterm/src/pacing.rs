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

use std::time::{Duration, Instant};

/// Most output-driven frames per second. Every message redraws every window, so this bounds
/// each window too.
pub const OUTPUT_FPS: u32 = 60;

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
            interval: Duration::from_secs(1) / fps.max(1),
            last: None,
            pending: Vec::new(),
            deadline: None,
        }
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
    fn fire_with_nothing_waiting_delivers_nothing() {
        let mut p: Pacer<u32> = Pacer::new(60);
        assert!(p.fire(Instant::now()).is_empty());
    }
}
