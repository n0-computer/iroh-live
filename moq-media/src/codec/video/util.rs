use std::time::Duration;

use hang::Timestamp;

pub(crate) mod annexb;
pub(crate) mod convert;
pub(crate) mod mjpg;
pub(crate) mod scale;

/// Tracks inter-frame delay from stream timestamps.
#[derive(Default, Debug)]
pub(crate) struct StreamClock {
    pub(crate) last_timestamp: Option<Timestamp>,
}

impl StreamClock {
    pub(crate) fn frame_delay(&mut self, timestamp: &Timestamp) -> Duration {
        let delay = match self.last_timestamp {
            None => Duration::ZERO,
            Some(last) => timestamp
                .checked_sub(last)
                .unwrap_or(Timestamp::ZERO)
                .into(),
        };
        self.last_timestamp = Some(*timestamp);
        delay
    }
}
