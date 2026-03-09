use std::time::Duration;

/// Tracks inter-frame delay from stream timestamps.
#[derive(Default, Debug)]
pub(crate) struct StreamClock {
    pub(crate) last_timestamp: Option<Duration>,
}

impl StreamClock {
    pub(crate) fn frame_delay(&mut self, timestamp: Duration) -> Duration {
        let delay = match self.last_timestamp {
            None => Duration::ZERO,
            Some(last) => timestamp.saturating_sub(last),
        };
        self.last_timestamp = Some(timestamp);
        delay
    }
}
