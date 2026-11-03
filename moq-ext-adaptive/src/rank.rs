//! Ordering a ladder of renditions, best first.

use std::cmp::Reverse;

/// Sorts `items` best first, by coded pixel count and then by bitrate.
///
/// `key` returns `(pixels, bitrate)` for an item. A missing bitrate ranks below
/// any known one, and the sort is stable, so items that tie keep their order.
pub fn rank<T>(items: &mut [T], key: impl Fn(&T) -> (u64, Option<u64>)) {
    items.sort_by_key(|item| Reverse(key(item)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn largest_first_then_richest() {
        let mut items = vec![
            ("low", 640 * 360, Some(500_000)),
            ("high", 1920 * 1080, Some(4_000_000)),
            ("720p-cheap", 1280 * 720, Some(800_000)),
            ("720p-rich", 1280 * 720, Some(3_000_000)),
        ];
        rank(&mut items, |(_, pixels, bitrate)| (*pixels, *bitrate));
        let names: Vec<_> = items.iter().map(|(name, ..)| *name).collect();
        assert_eq!(names, ["high", "720p-rich", "720p-cheap", "low"]);
    }
}
