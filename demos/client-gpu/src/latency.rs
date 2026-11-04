//! Rolling latency statistics.

use std::{collections::VecDeque, time::Duration};

/// How many samples are kept.
const KEEP: usize = 600;

/// The most recent latency samples, in milliseconds.
#[derive(Debug, Default)]
pub struct LatencyStats {
    samples: VecDeque<f32>,
}

impl LatencyStats {
    /// Adds one sample, dropping the oldest beyond the window.
    pub fn push(&mut self, latency: Duration) {
        if self.samples.len() == KEEP {
            self.samples.pop_front();
        }
        self.samples.push_back(latency.as_secs_f32() * 1000.0);
    }

    /// Returns the newest sample.
    pub fn current(&self) -> Option<f32> {
        self.samples.back().copied()
    }

    /// Returns the `p`th percentile of the window, for `p` from 0 to 100.
    pub fn percentile(&self, p: f32) -> Option<f32> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<f32> = self.samples.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let rank = (p / 100.0 * (sorted.len() - 1) as f32).round() as usize;
        Some(sorted[rank])
    }

    /// Returns the samples, oldest first.
    pub fn samples(&self) -> impl Iterator<Item = f32> + '_ {
        self.samples.iter().copied()
    }

    /// Returns how many samples are in the window.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Returns whether the window is empty.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Formats the current value and the percentiles for a label.
    pub fn summary(&self) -> String {
        let Some(now) = self.current() else {
            return "no samples yet".to_string();
        };
        format!(
            "now {now:.0} ms  p50 {:.0} ms  p95 {:.0} ms  ({} samples)",
            self.percentile(50.0).unwrap_or_default(),
            self.percentile(95.0).unwrap_or_default(),
            self.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_follow_the_window() {
        let mut stats = LatencyStats::default();
        for ms in 1..=100u64 {
            stats.push(Duration::from_millis(ms));
        }
        assert_eq!(stats.current().map(f32::round), Some(100.0));
        assert_eq!(stats.percentile(50.0).map(f32::round), Some(51.0));
        assert_eq!(stats.percentile(95.0).map(f32::round), Some(95.0));
    }
}
