//! The CPU filters the gpu node applies, selected by the client.

/// A filter over a whole I420 picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Passes the picture through.
    #[default]
    None,
    /// Inverts every sample, chroma included, so the colors invert too.
    Invert,
    /// A box blur, radius 4 on luma and 2 on chroma.
    Blur,
}

impl Filter {
    /// Every filter, in menu order.
    pub const ALL: [Self; 3] = [Self::None, Self::Invert, Self::Blur];

    /// Returns the name used on the control track and on screen.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Invert => "invert",
            Self::Blur => "blur",
        }
    }

    /// Parses a name from [`name`](Self::name).
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|filter| filter.name() == name)
    }

    /// Applies the filter to the packed I420 planes `data` of `width` by `height`.
    pub fn apply(self, data: &mut [u8], width: usize, height: usize) {
        match self {
            Self::None => {}
            Self::Invert => data.iter_mut().for_each(|sample| *sample = 255 - *sample),
            Self::Blur => {
                let luma = width * height;
                let (y, chroma) = data.split_at_mut(luma);
                box_blur(y, width, height, 4);
                let (u, v) = chroma.split_at_mut(luma / 4);
                box_blur(u, width / 2, height / 2, 2);
                box_blur(v, width / 2, height / 2, 2);
            }
        }
    }
}

/// Blurs one plane in place with a box of `2 * radius + 1` samples per axis.
fn box_blur(plane: &mut [u8], width: usize, height: usize, radius: usize) {
    let mut scratch = vec![0u8; plane.len()];
    // Along the rows, then down the columns.
    blur_lines(plane, &mut scratch, height, width, width, 1, radius);
    blur_lines(&scratch, plane, width, 1, height, width, radius);
}

/// Blurs `lines` lines of `len` samples each, a sliding window per line.
///
/// Line `l` starts at `l * line_stride` and its samples are `step` apart.
fn blur_lines(
    src: &[u8],
    dst: &mut [u8],
    lines: usize,
    line_stride: usize,
    len: usize,
    step: usize,
    radius: usize,
) {
    for line in 0..lines {
        let at = |i: usize| line * line_stride + i * step;
        let mut sum: u32 = (0..=radius.min(len - 1))
            .map(|i| u32::from(src[at(i)]))
            .sum();
        for i in 0..len {
            let (lo, hi) = (i.saturating_sub(radius), (i + radius).min(len - 1));
            dst[at(i)] = (sum / (hi - lo + 1) as u32) as u8;
            if i + radius + 1 < len {
                sum += u32::from(src[at(i + radius + 1)]);
            }
            if i >= radius {
                sum -= u32::from(src[at(i - radius)]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_a_flat_plane_flat() {
        let mut plane = vec![100u8; 16 * 8];
        box_blur(&mut plane, 16, 8, 3);
        assert!(plane.iter().all(|sample| *sample == 100));
    }

    #[test]
    fn blur_spreads_an_edge() {
        let mut plane = vec![0u8; 16];
        plane[8..].fill(200);
        box_blur(&mut plane, 16, 1, 2);
        assert!(plane[7] > 0 && plane[7] < 200);
        assert_eq!((plane[0], plane[15]), (0, 200));
    }

    #[test]
    fn names_round_trip() {
        for filter in Filter::ALL {
            assert_eq!(Filter::parse(filter.name()), Some(filter));
        }
    }
}
