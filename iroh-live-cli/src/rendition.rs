//! The `--renditions` grammar, and the capture frame rate it settles.
//!
//! A rung is `[<name>:]<geometry>[@<fps>]`, where the geometry is `<height>p`
//! or `<width>x<height>`. A rung with no geometry is a name for the source's
//! own resolution.
//!
//! One capture feeds every encoder of a ladder, so `@<fps>` is a capture rate.
//! The highest one wins, and `--fps` overrides them all.

use std::time::Duration;

use iroh_live::media::{
    Bitrate, VideoEncoding, VideoRendition,
    video::{self, Size},
};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

use crate::{args::CaptureArgs, source_spec::VideoSourceSpec};

/// The capture frame rate when nothing asks for one.
///
/// A backend picks its nearest rate, so this gives at most 30 without a mode
/// list. It matches the screen capture backends' own default.
pub const DEFAULT_FRAMERATE: u32 = 30;

/// The highest frame rate `--fps` or an `@<fps>` suffix may name.
///
/// The PipeWire backend's ceiling. A larger number is a typo.
const MAX_FRAMERATE: u32 = 1_000;

/// The rendition name a publish without `--renditions` uses.
const SINGLE_RENDITION: &str = "video";

/// The capture frame rate a publish runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFramerate {
    /// The rate the backend is asked for.
    ///
    /// A device may substitute its nearest rate.
    Requested {
        /// Frames per second.
        fps: u32,
        /// What settled the number.
        origin: FramerateOrigin,
    },
    /// The backend runs at its own rate.
    Device,
}

impl CaptureFramerate {
    /// Returns the rate for a capture config, or `None` for the backend's own.
    pub fn request(self) -> Option<u32> {
        match self {
            Self::Requested { fps, .. } => Some(fps),
            Self::Device => None,
        }
    }

    /// Returns the rate for a source that generates its own frames.
    ///
    /// The test pattern and `rpicam-vid` have no device mode to defer to, so
    /// [`DEFAULT_FRAMERATE`] stands in.
    pub fn generated(self) -> u32 {
        self.request().unwrap_or(DEFAULT_FRAMERATE)
    }
}

/// What settled a requested capture frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramerateOrigin {
    /// `--fps` named it.
    Flag,
    /// The highest `@<fps>` in `--renditions` named it.
    Renditions,
    /// `--fps` named it, below what a rung asked for.
    Capped,
    /// Nothing named a rate, so [`DEFAULT_FRAMERATE`] applies.
    Default,
}

impl FramerateOrigin {
    /// Returns the `origin` log field.
    fn label(self) -> &'static str {
        match self {
            Self::Flag => "--fps",
            Self::Renditions => "--renditions",
            Self::Capped => "--fps, capping the ladder",
            Self::Default => "default",
        }
    }
}

/// A rung that asked for a frame rate the capture does not run at.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnmetRate {
    /// The rendition name.
    name: String,
    /// The rate its `@<fps>` asked for.
    asked: u32,
}

/// One rung of `--renditions`, as it was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rung {
    /// The catalog name for this rendition.
    name: String,
    /// The encoded size, or `None` for the source's own resolution.
    size: Option<Size>,
    /// The rate its `@<fps>` asked for, if any.
    framerate: Option<u32>,
}

/// The simulcast ladder and its capture frame rate.
#[derive(Debug)]
pub struct Ladder {
    /// The rungs, ready for `set_video`.
    pub encoding: VideoEncoding,
    /// The rate the capture backend is asked for.
    pub framerate: CaptureFramerate,
    /// The rungs whose `@<fps>` differs from the capture rate.
    unmet: Vec<UnmetRate>,
}

impl Ladder {
    /// Logs the capture frame rate and the rungs that asked for another.
    ///
    /// Call it only once a video source opened.
    pub fn report(&self) {
        match self.framerate {
            CaptureFramerate::Requested { fps, origin } => info!(
                fps,
                origin = origin.label(),
                "capture frame rate requested; the device runs at the nearest rate it supports",
            ),
            CaptureFramerate::Device => info!(
                origin = "device",
                "capture frame rate left to the device, which this backend picks itself",
            ),
        }
        let Some(fps) = self.framerate.request() else {
            return;
        };
        for rung in &self.unmet {
            warn!(
                rendition = %rung.name,
                asked = rung.asked,
                fps,
                "a ladder is captured once, so this rung encodes at the capture frame rate \
                 rather than the rate it asked for",
            );
        }
    }
}

/// Builds the ladder and capture frame rate for `spec` from `args`.
///
/// Fails if a rung does not parse or a frame rate is out of range.
pub fn ladder(spec: &VideoSourceSpec, args: &CaptureArgs) -> Result<Ladder> {
    let rungs = rungs(args)?;
    let framerate = capture_framerate(spec, args, &rungs)?;
    Ok(Ladder {
        encoding: VideoEncoding::ladder(renditions(args, &rungs)),
        unmet: unmet_rates(&rungs, framerate),
        framerate,
    })
}

/// Parses `--renditions`, or returns one unscaled rung when it is empty.
fn rungs(args: &CaptureArgs) -> Result<Vec<Rung>> {
    if args.renditions.is_empty() {
        return Ok(vec![Rung {
            name: SINGLE_RENDITION.to_string(),
            size: None,
            framerate: None,
        }]);
    }
    args.renditions.iter().map(|spec| rung(spec)).collect()
}

/// Parses one rung: `[<name>:]<geometry>[@<fps>]`.
fn rung(spec: &str) -> Result<Rung> {
    // Splitting on the first `@` makes `a@b@60` fail on `b@60` instead of
    // accepting `a@b` as a name.
    let (geometry, framerate) = match spec.split_once('@') {
        Some((geometry, fps)) => (geometry, Some(rung_framerate(spec, fps)?)),
        None => (spec, None),
    };
    let (name, size) = match geometry.split_once(':') {
        Some((name, size)) => {
            let parsed = parse_size(size).ok_or_else(|| {
                anyerr!("rendition '{spec}': '{size}' is not <height>p or <width>x<height>")
            })?;
            (name.to_string(), Some(parsed))
        }
        // A bare size names itself. Anything else is a name at source size.
        None => (geometry.to_string(), parse_size(geometry)),
    };
    if name.is_empty() {
        return Err(anyerr!(
            "rendition '{spec}': a rung needs a name or a size of its own"
        ));
    }
    Ok(Rung {
        name,
        size,
        framerate,
    })
}

/// Parses the `@<fps>` suffix of `spec`.
fn rung_framerate(spec: &str, value: &str) -> Result<u32> {
    let fps: u32 = value
        .parse()
        .map_err(|_| anyerr!("rendition '{spec}': '{value}' is not a frame rate"))?;
    if !(1..=MAX_FRAMERATE).contains(&fps) {
        return Err(anyerr!(
            "rendition '{spec}': {fps} frames per second is outside the 1 to \
             {MAX_FRAMERATE} a capture backend accepts"
        ));
    }
    Ok(fps)
}

/// Checks the rate `--fps` names.
fn flag_framerate(fps: u32) -> Result<u32> {
    if !(1..=MAX_FRAMERATE).contains(&fps) {
        return Err(anyerr!(
            "--fps {fps} is outside the 1 to {MAX_FRAMERATE} a capture backend accepts"
        ));
    }
    Ok(fps)
}

/// Settles the capture frame rate and its origin.
fn capture_framerate(
    spec: &VideoSourceSpec,
    args: &CaptureArgs,
    rungs: &[Rung],
) -> Result<CaptureFramerate> {
    let asked = rungs.iter().filter_map(|rung| rung.framerate).max();
    let flag = args.fps.map(flag_framerate).transpose()?;
    let framerate = match (flag, asked) {
        (Some(fps), Some(asked)) if asked > fps => CaptureFramerate::Requested {
            fps,
            origin: FramerateOrigin::Capped,
        },
        (Some(fps), _) => CaptureFramerate::Requested {
            fps,
            origin: FramerateOrigin::Flag,
        },
        (None, Some(fps)) => CaptureFramerate::Requested {
            fps,
            origin: FramerateOrigin::Renditions,
        },
        (None, None) if takes_framerate(spec) => CaptureFramerate::Requested {
            fps: DEFAULT_FRAMERATE,
            origin: FramerateOrigin::Default,
        },
        (None, None) => CaptureFramerate::Device,
    };
    Ok(framerate)
}

/// Returns whether the backend behind `spec` acts on a requested frame rate.
///
/// The AVFoundation camera backend ignores it and warns. We skip the default
/// there to avoid that warning, but still pass an explicit `--fps`.
fn takes_framerate(spec: &VideoSourceSpec) -> bool {
    !(cfg!(target_os = "macos") && matches!(spec, VideoSourceSpec::Camera(_)))
}

/// Returns the rungs whose `@<fps>` differs from the capture rate.
fn unmet_rates(rungs: &[Rung], framerate: CaptureFramerate) -> Vec<UnmetRate> {
    // No request means no rung named a rate.
    let Some(fps) = framerate.request() else {
        return Vec::new();
    };
    rungs
        .iter()
        .filter_map(|rung| {
            let asked = rung.framerate?;
            (asked != fps).then(|| UnmetRate {
                name: rung.name.clone(),
                asked,
            })
        })
        .collect()
}

/// Turns parsed rungs into the renditions `set_video` encodes.
fn renditions(args: &CaptureArgs, rungs: &[Rung]) -> Vec<VideoRendition> {
    let codec = args.codec.into();
    let kind = video::encode::Kind::from(args.encoder);

    // `--bitrate` is for the largest rung. An unsized rung is at source size,
    // which is unknown here, so there is nothing to scale against.
    let largest = match rungs.iter().any(|rung| rung.size.is_none()) {
        true => None,
        false => rungs
            .iter()
            .filter_map(|rung| rung.size)
            .max_by_key(Size::pixels),
    };

    rungs
        .iter()
        .map(|rung| {
            let mut rendition = VideoRendition {
                size: rung.size,
                codec,
                encoder: kind.clone(),
                ..VideoRendition::new(rung.name.clone())
            };
            if args.keyframe_interval > 0.0 {
                rendition.keyframe_interval = Duration::from_secs_f64(args.keyframe_interval);
            }
            // A subscriber picks a rung by comparing its bandwidth estimate
            // with each rung's bitrate, so the rungs must differ.
            rendition.bitrate = args
                .bitrate
                .map(|bitrate| Bitrate::from_bps(scaled_bitrate(bitrate, rung.size, largest)));
            rendition
        })
        .collect()
}

/// Shares `bitrate` across a ladder in proportion to pixel count.
///
/// `bitrate` is for the largest rung. The result has a floor, so a tiny rung
/// still gets a usable rate.
fn scaled_bitrate(bitrate: u64, size: Option<Size>, largest: Option<Size>) -> u64 {
    /// Below this, no rung is usable.
    const FLOOR: u64 = 64_000;

    let (Some(size), Some(largest)) = (size, largest) else {
        return bitrate;
    };
    match largest.pixels() {
        0 => bitrate,
        total => (bitrate * size.pixels() / total).max(FLOOR),
    }
}

/// Parses `<height>p` or `<width>x<height>`.
///
/// Rounds both dimensions up to even, since I420 chroma is subsampled 2x2.
/// `<height>p` assumes 16:9.
fn parse_size(spec: &str) -> Option<Size> {
    if let Some((width, height)) = spec.split_once(['x', 'X']) {
        let width: u32 = width.parse().ok()?;
        let height: u32 = height.parse().ok()?;
        return Some(Size::new(even(width), even(height)));
    }
    let height: u32 = spec.strip_suffix(['p', 'P'])?.parse().ok()?;
    let width = u32::try_from((u64::from(height) * 16 + 4) / 9).ok()?;
    Some(Size::new(even(width), even(height)))
}

/// Rounds up to the next even number.
fn even(value: u32) -> u32 {
    value + (value % 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture flags carrying nothing but a ladder.
    fn args(renditions: &[&str], fps: Option<u32>) -> CaptureArgs {
        CaptureArgs {
            renditions: renditions.iter().map(|spec| (*spec).to_string()).collect(),
            fps,
            ..CaptureArgs::default()
        }
    }

    /// A source that takes a requested frame rate on every platform.
    const TEST_SOURCE: VideoSourceSpec =
        VideoSourceSpec::Test(crate::source_spec::TestPattern::Timing);

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("720p"), Some(Size::new(1280, 720)));
        assert_eq!(parse_size("1080p"), Some(Size::new(1920, 1080)));
        assert_eq!(parse_size("480p"), Some(Size::new(854, 480)));
        assert_eq!(parse_size("640x360"), Some(Size::new(640, 360)));
        // Odd dimensions round up to even.
        assert_eq!(parse_size("641x361"), Some(Size::new(642, 362)));
        assert_eq!(parse_size("source"), None);
    }

    #[test]
    fn ladder_bitrates_scale_by_pixel_count() {
        let quarter = scaled_bitrate(
            4_000_000,
            Some(Size::new(640, 360)),
            Some(Size::new(1280, 720)),
        );
        assert_eq!(quarter, 1_000_000);

        let top = scaled_bitrate(
            4_000_000,
            Some(Size::new(1280, 720)),
            Some(Size::new(1280, 720)),
        );
        assert_eq!(top, 4_000_000);

        let tiny = scaled_bitrate(
            4_000_000,
            Some(Size::new(16, 16)),
            Some(Size::new(1920, 1080)),
        );
        assert_eq!(tiny, 64_000);

        assert_eq!(scaled_bitrate(4_000_000, None, None), 4_000_000);
    }

    #[test]
    fn rendition_rungs_parse() {
        assert_eq!(
            rung("720p").unwrap(),
            Rung {
                name: "720p".to_string(),
                size: Some(Size::new(1280, 720)),
                framerate: None,
            }
        );
        assert_eq!(
            rung("low:640x360").unwrap(),
            Rung {
                name: "low".to_string(),
                size: Some(Size::new(640, 360)),
                framerate: None,
            }
        );
        assert_eq!(
            rung("source").unwrap(),
            Rung {
                name: "source".to_string(),
                size: None,
                framerate: None,
            }
        );
        assert!(rung("low:wide").is_err());
    }

    #[test]
    fn a_rung_takes_a_frame_rate_after_its_size() {
        assert_eq!(
            rung("720p@60").unwrap(),
            Rung {
                name: "720p".to_string(),
                size: Some(Size::new(1280, 720)),
                framerate: Some(60),
            }
        );
        assert_eq!(
            rung("high:1280x720@60").unwrap(),
            Rung {
                name: "high".to_string(),
                size: Some(Size::new(1280, 720)),
                framerate: Some(60),
            }
        );
        assert_eq!(
            rung("source@24").unwrap(),
            Rung {
                name: "source".to_string(),
                size: None,
                framerate: Some(24),
            }
        );
    }

    #[test]
    fn a_rung_with_an_unusable_frame_rate_is_refused() {
        assert!(rung("720p@").is_err());
        assert!(rung("720p@sixty").is_err());
        assert!(rung("720p@0").is_err());
        assert!(rung(&format!("720p@{}", MAX_FRAMERATE + 1)).is_err());
        assert!(rung("a@b@60").is_err());
        assert!(rung("@60").is_err());
        assert!(rung("high:@60").is_err());
    }

    #[test]
    fn a_ladder_without_a_rate_captures_at_the_default() {
        let ladder = ladder(&TEST_SOURCE, &args(&["720p", "low:640x360"], None)).unwrap();
        assert_eq!(
            ladder.framerate,
            CaptureFramerate::Requested {
                fps: DEFAULT_FRAMERATE,
                origin: FramerateOrigin::Default,
            }
        );
        assert!(ladder.unmet.is_empty());
    }

    #[test]
    fn the_highest_rung_rate_sets_the_capture_rate() {
        let ladder = ladder(
            &TEST_SOURCE,
            &args(&["high:1280x720@60", "low:640x360@30"], None),
        )
        .unwrap();
        assert_eq!(
            ladder.framerate,
            CaptureFramerate::Requested {
                fps: 60,
                origin: FramerateOrigin::Renditions,
            }
        );
        assert_eq!(
            ladder.unmet,
            vec![UnmetRate {
                name: "low".to_string(),
                asked: 30,
            }]
        );
    }

    #[test]
    fn the_fps_flag_outranks_the_ladder() {
        let ladder = ladder(&TEST_SOURCE, &args(&["720p@30"], Some(60))).unwrap();
        assert_eq!(
            ladder.framerate,
            CaptureFramerate::Requested {
                fps: 60,
                origin: FramerateOrigin::Flag,
            }
        );
        assert_eq!(
            ladder.unmet,
            vec![UnmetRate {
                name: "720p".to_string(),
                asked: 30,
            }]
        );
    }

    #[test]
    fn the_fps_flag_caps_a_rung_asking_for_more() {
        let ladder = ladder(&TEST_SOURCE, &args(&["720p@60"], Some(30))).unwrap();
        assert_eq!(
            ladder.framerate,
            CaptureFramerate::Requested {
                fps: 30,
                origin: FramerateOrigin::Capped,
            }
        );
        assert_eq!(
            ladder.unmet,
            vec![UnmetRate {
                name: "720p".to_string(),
                asked: 60,
            }]
        );
    }

    #[test]
    fn a_rung_that_matches_the_capture_rate_is_not_reported() {
        let ladder = ladder(&TEST_SOURCE, &args(&["720p@60", "low:640x360@60"], None)).unwrap();
        assert_eq!(ladder.framerate.request(), Some(60));
        assert!(ladder.unmet.is_empty());
    }

    #[test]
    fn an_unusable_fps_flag_is_refused() {
        assert!(ladder(&TEST_SOURCE, &args(&[], Some(0))).is_err());
        assert!(ladder(&TEST_SOURCE, &args(&[], Some(MAX_FRAMERATE + 1))).is_err());
    }

    #[test]
    fn a_ladder_without_the_flag_is_one_unscaled_rendition() {
        let ladder = ladder(&TEST_SOURCE, &args(&[], None)).unwrap();
        assert_eq!(ladder.encoding.renditions.len(), 1);
        assert_eq!(ladder.encoding.renditions[0].name, SINGLE_RENDITION);
        assert_eq!(ladder.encoding.renditions[0].size, None);
    }

    #[test]
    fn a_rate_does_not_reach_the_rendition_the_encoder_is_built_from() {
        let ladder = ladder(&TEST_SOURCE, &args(&["high:1280x720@60"], None)).unwrap();
        assert_eq!(ladder.encoding.renditions.len(), 1);
        assert_eq!(ladder.encoding.renditions[0].name, "high");
        assert_eq!(
            ladder.encoding.renditions[0].size,
            Some(Size::new(1280, 720))
        );
    }
}
