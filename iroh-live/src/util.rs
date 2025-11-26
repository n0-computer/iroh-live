use std::time::{Duration, Instant};

use byte_unit::{Bit, UnitType};
use iroh::endpoint::ConnectionStats;

pub struct StatsSmoother {
    last_bytes: u64,
    last_update: Instant,
    rate: String,
    rtt: Duration,
}

impl StatsSmoother {
    pub fn new() -> Self {
        Self {
            last_bytes: 0,
            last_update: Instant::now(),
            rate: "0.00 bit/s".into(),
            rtt: Duration::from_secs(0),
        }
    }
    pub fn smoothed(&mut self, total: impl FnOnce() -> ConnectionStats) -> (Duration, &str) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update);
        if elapsed >= Duration::from_secs(1) {
            let stats = (total)();
            let total = stats.udp_rx.bytes;
            let delta = total.saturating_sub(self.last_bytes);
            let secs = elapsed.as_secs_f64();
            let bps = if secs > 0.0 && delta > 0 {
                (delta as f64 * 8.0) / secs
            } else {
                0.0
            };
            let bit = Bit::from_f64(bps).unwrap();
            let adjusted = bit.get_appropriate_unit(UnitType::Decimal);
            self.rate = format!("{adjusted:.2}/s");
            self.last_update = now;
            self.last_bytes = total;
            self.rtt = stats.path.rtt;
        }
        (self.rtt, &self.rate)
    }
}
