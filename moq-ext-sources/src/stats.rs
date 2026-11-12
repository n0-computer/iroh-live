//! Small helpers for the figures a broadcast and a player report.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// A value one task writes and anyone snapshots.
///
/// Clones share the value.
#[derive(Debug, Clone, Default)]
pub struct SharedCell<T>(Arc<Mutex<T>>);

impl<T: Clone> SharedCell<T> {
    /// Changes the value in place.
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.0.lock().expect("poisoned"));
    }

    /// Returns a copy of the value.
    #[must_use]
    pub fn get(&self) -> T {
        self.0.lock().expect("poisoned").clone()
    }
}

/// How long a [`RateMeter`] counts events before it reports.
///
/// Frame rates are quoted per second. A meter reports only when an event
/// arrives, so a stream that stops keeps its last rate.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// A counter of events that reports their rate once per window.
///
/// Owned by the one task that sees the events, so it needs no lock.
#[derive(Debug)]
pub struct RateMeter {
    window: Duration,
    started: Instant,
    events: u32,
    amount: u64,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::over(RATE_WINDOW)
    }
}

impl RateMeter {
    /// Creates a meter that reports once per `window`.
    #[must_use]
    pub fn over(window: Duration) -> Self {
        Self::starting(window, Instant::now())
    }

    /// Creates a meter whose window opened at `started`.
    fn starting(window: Duration, started: Instant) -> Self {
        Self {
            window,
            started,
            events: 0,
            amount: 0,
        }
    }

    /// Counts one event carrying `amount`, such as bytes.
    ///
    /// Returns the rates when this event closes the window, and `None` before
    /// that.
    #[must_use]
    pub fn tick(&mut self, amount: u64) -> Option<Rates> {
        self.tick_at(amount, Instant::now())
    }

    /// Counts one event at `now`, as [`tick`](Self::tick) does.
    fn tick_at(&mut self, amount: u64, now: Instant) -> Option<Rates> {
        self.events += 1;
        self.amount += amount;
        let elapsed = now.duration_since(self.started);
        if elapsed < self.window {
            return None;
        }
        let seconds = elapsed.as_secs_f64();
        let rates = Rates {
            events: f64::from(self.events) / seconds,
            amount: self.amount as f64 / seconds,
        };
        // The window restarts at the reading, so time spent computing does not
        // add up across windows.
        *self = Self::starting(self.window, now);
        Some(rates)
    }
}

/// What a [`RateMeter`] counted over one window, per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// Events per second.
    pub events: f64,
    /// The amount the events carried, per second.
    pub amount: f64,
}

/// An exponentially smoothed duration, for timings that jitter per call.
#[derive(Debug, Default)]
pub struct Smoothed(Option<f64>);

impl Smoothed {
    /// The weight of a new sample.
    const ALPHA: f64 = 0.2;

    /// Folds `sample` in and returns the smoothed value.
    pub fn record(&mut self, sample: Duration) -> Duration {
        let value = sample.as_secs_f64();
        let smoothed = match self.0 {
            Some(previous) => Self::ALPHA * value + (1.0 - Self::ALPHA) * previous,
            None => value,
        };
        self.0 = Some(smoothed);
        Duration::from_secs_f64(smoothed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs forty events at offsets from `offset` and returns the first rate.
    fn drive(offset: impl Fn(u32) -> Duration) -> f64 {
        let start = Instant::now();
        let mut meter = RateMeter::starting(RATE_WINDOW, start);
        (0..40)
            .find_map(|event| meter.tick_at(0, start + offset(event)))
            .expect("the offsets span the window")
            .events
    }

    /// Bursty delivery reads the same rate as even delivery.
    #[test]
    fn a_burst_reads_the_same_as_an_even_stream() {
        let bursty = drive(|event| Duration::from_millis(350) * (event / 10));
        let even = drive(|event| Duration::from_millis(35) * event);
        assert!((bursty - even).abs() < 0.5, "{bursty} against {even}");
        assert!((28.0..32.0).contains(&bursty), "{bursty}");
    }

    #[test]
    fn the_amount_is_a_rate_too() {
        let start = Instant::now();
        let mut meter = RateMeter::starting(RATE_WINDOW, start);
        // Thirty events 33 ms apart stay inside the window.
        for tick in 0..30u32 {
            let rate = meter.tick_at(2_500, start + Duration::from_millis(33) * tick);
            assert_eq!(rate, None, "the window closed early at event {tick}");
        }
        // 31 events of 2500 bytes over one second.
        let bytes = meter
            .tick_at(2_500, start + Duration::from_secs(1))
            .expect("a second has passed")
            .amount;
        assert!((77_000.0..78_000.0).contains(&bytes), "{bytes}");
    }

    #[test]
    fn smoothing_starts_at_the_first_sample() {
        let mut smoothed = Smoothed::default();
        assert_eq!(
            smoothed.record(Duration::from_millis(10)),
            Duration::from_millis(10)
        );
        let next = smoothed.record(Duration::from_millis(20));
        assert!(next > Duration::from_millis(10) && next < Duration::from_millis(20));
    }
}
