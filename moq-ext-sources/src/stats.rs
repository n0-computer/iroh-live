//! Small helpers for the figures a broadcast and a player report.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::time::Instant;

/// A value one task writes and anyone snapshots.
///
/// Clones share the value.
// Shared with moq-ext-publisher and moq-ext-player, not public API.
#[doc(hidden)]
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
/// arrives, so a reader asks [`Rates::is_current`] whether a stream still runs.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// A counter of events that reports their rate once per window.
///
/// Owned by the one task that sees the events, so it needs no lock. It reads
/// the clock through tokio, so a test with paused time drives it.
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
        let elapsed = now.duration_since(self.started);
        if elapsed >= self.window * Rates::CURRENT_FOR {
            // The stream paused for so long that the last reading went stale.
            // Averaging over the pause would report a rate far below the one
            // the stream resumes at, so this event opens a new window instead,
            // as the event that closes a window does.
            *self = Self::starting(self.window, now);
            return None;
        }
        self.events += 1;
        self.amount += amount;
        if elapsed < self.window {
            return None;
        }
        let seconds = elapsed.as_secs_f64();
        let rates = Rates {
            events: f64::from(self.events) / seconds,
            amount: self.amount as f64 / seconds,
            at: now,
            window: self.window,
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
    /// When the window closed.
    at: Instant,
    /// The meter's window.
    window: Duration,
}

impl Rates {
    /// How many windows a reading stays current for.
    ///
    /// A steady stream closes a window about once per window, and a stream
    /// slower than one event per window closes one with every event. Two
    /// windows without a reading mean at least one window without an event,
    /// so a stream reads as running only while it sends faster than one event
    /// per two windows.
    const CURRENT_FOR: u32 = 2;

    /// Returns whether the stream still runs, as far as these rates tell.
    ///
    /// The rates go stale once the meter closed no window for two windows. A
    /// reader shows a stale rate as unknown, since the stream stopped.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.is_current_at(Instant::now())
    }

    /// Returns whether the rates are current at `now`, as [`is_current`](Self::is_current) does.
    fn is_current_at(&self, now: Instant) -> bool {
        now.duration_since(self.at) < self.window * Self::CURRENT_FOR
    }
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

    /// Rates stay current while the stream runs and go stale once it stops.
    #[test]
    fn rates_go_stale_once_events_stop() {
        let start = Instant::now();
        let mut meter = RateMeter::starting(RATE_WINDOW, start);
        let mut last = None;
        // Three seconds at 20 events per second.
        for event in 1..=60u32 {
            let now = start + Duration::from_millis(50) * event;
            if let Some(rates) = meter.tick_at(0, now) {
                last = Some(rates);
            }
            let rates = last.filter(|rates| rates.is_current_at(now));
            if now >= start + RATE_WINDOW {
                let rates = rates.expect("a running stream has a rate");
                assert!((19.0..21.0).contains(&rates.events), "{}", rates.events);
            }
        }
        let rates = last.expect("the stream closed windows");
        let stopped = start + Duration::from_secs(3);
        assert!(rates.is_current_at(stopped + RATE_WINDOW));
        assert!(!rates.is_current_at(stopped + RATE_WINDOW * 2));

        // After a five-second pause the stream resumes at 20 events per second.
        // The first reading covers only the time since the resume.
        let resumed = stopped + Duration::from_secs(5);
        let rates = (0..=40u32)
            .find_map(|event| meter.tick_at(0, resumed + Duration::from_millis(50) * event))
            .expect("the resumed stream closes a window");
        assert_eq!(rates.at, resumed + RATE_WINDOW);
        assert!((19.0..21.0).contains(&rates.events), "{}", rates.events);
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
