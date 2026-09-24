//! Bit rates, in a type of our own.

use std::fmt;

/// A rate in bits per second.
///
/// Our own newtype rather than moq-net's `bandwidth::Rate`, so a public
/// signature does not move when that one does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bitrate(u64);

impl Bitrate {
    /// Creates a rate of `bps` bits per second.
    pub const fn from_bps(bps: u64) -> Self {
        Self(bps)
    }

    /// Creates a rate of `kbps` thousand bits per second.
    pub const fn from_kbps(kbps: u64) -> Self {
        Self(kbps * 1_000)
    }

    /// Creates a rate of `mbps` million bits per second.
    pub const fn from_mbps(mbps: u64) -> Self {
        Self(mbps * 1_000_000)
    }

    /// Returns the rate in bits per second.
    pub const fn as_bps(self) -> u64 {
        self.0
    }

    /// Returns the rate in thousands of bits per second, rounded down.
    pub const fn as_kbps(self) -> u64 {
        self.0 / 1_000
    }

    /// Returns the rate as moq-net spells it.
    pub(crate) fn to_moq(self) -> moq_net::bandwidth::Rate {
        moq_net::bandwidth::Rate::from_bps(self.0)
    }
}

/// Formats as `1.5 Mbit/s`, `320 kbit/s` or `64 bit/s`.
impl fmt::Display for Bitrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bps = self.0 as f64;
        if bps >= 1_000_000.0 {
            write!(f, "{:.1} Mbit/s", bps / 1_000_000.0)
        } else if bps >= 1_000.0 {
            write!(f, "{:.0} kbit/s", bps / 1_000.0)
        } else {
            write!(f, "{} bit/s", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bitrate_prints_in_the_unit_that_reads_best() {
        assert_eq!(Bitrate::from_bps(1_500_000).to_string(), "1.5 Mbit/s");
        assert_eq!(Bitrate::from_kbps(320).to_string(), "320 kbit/s");
        assert_eq!(Bitrate::from_bps(64).to_string(), "64 bit/s");
    }

    #[test]
    fn the_units_convert() {
        assert_eq!(Bitrate::from_mbps(2).as_kbps(), 2_000);
        assert_eq!(Bitrate::from_kbps(3).as_bps(), 3_000);
    }
}
