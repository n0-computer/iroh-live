//! The `--renditions` grammar, and the capture frame rate it settles.
//!
//! A rung is `[<name>:]<geometry>[@<fps>]`, so `720p`, `low:640x360` and
//! `high:1280x720@60` are all rungs. The geometry is `<height>p` or
//! `<width>x<height>`; a rung with no geometry at all is a name standing for
//! the source's own resolution, which is what a single-rendition publish uses.
//!
//! `@<fps>` names a capture rate rather than a per-rung encode rate. Every rung
//! of a ladder is fed the same pictures: `moq_media` opens one capture, and
//! `fan_out` hands each frame to every encoder, so there is one frame rate for
//! the whole ladder and no rung can run slower than another. The rung that asks
//! for the most frames therefore sets the rate all of them are captured at, and
//! [`Ladder::report`] names any rung that asked for something else.
//!
//! `--fps` says the same thing about the whole publish and wins where the two
//! disagree: it is the capture rate, and a rung asking for more than it allows
//! gets what the capture actually runs at.

use std::time::Duration;

use iroh_live::media::{
    publish::VideoRendition,
    video::{self, Size},
};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

use crate::{args::CaptureArgs, source_spec::VideoSourceSpec};

/// The capture frame rate when nothing asks for one.
///
/// A capture backend substitutes the nearest rate it has for one it cannot
/// reach, so requesting this is what gives a publisher `min(30, whatever the
/// device supports)` without a mode list to consult. It is also the rate every
/// screen capture backend in `moq_video` already falls back to.
pub const DEFAULT_FRAMERATE: u32 = 30;

/// The highest frame rate `--fps` or an `@<fps>` suffix may name.
///
/// The ceiling the PipeWire backend offers a compositor, and well above any
/// camera mode. A larger number is a typo, and catching it here beats asking a
/// device for it.
const MAX_FRAMERATE: u32 = 1_000;

/// The rendition name a publish without `--renditions` uses.
const SINGLE_RENDITION: &str = "video";

/// The capture frame rate a publish runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFramerate {
    /// The rate the backend is asked for. A device that cannot reach it
    /// substitutes the nearest rate it has, which is why this is a request and
    /// not the rate the pictures actually arrive at.
    Requested {
        /// Frames per second.
        fps: u32,
        /// What settled the number.
        origin: FramerateOrigin,
    },
    /// Nothing is asked of the backend, which runs at its own rate.
    Device,
}

impl CaptureFramerate {
    /// Returns the rate to put in a capture config, or `None` to leave it to
    /// the backend.
    pub fn request(self) -> Option<u32> {
        match self {
            Self::Requested { fps, .. } => Some(fps),
            Self::Device => None,
        }
    }

    /// Returns the rate a source that produces its own frames should run at.
    ///
    /// The test pattern and `rpicam-vid` deliver exactly the rate they are
    /// given, so there is no device mode to defer to and [`DEFAULT_FRAMERATE`]
    /// stands in for one.
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
    /// A rung asked for more than `--fps` allows, so `--fps` settled it.
    Capped,
    /// Nothing named a rate, so [`DEFAULT_FRAMERATE`] applies.
    Default,
}

impl FramerateOrigin {
    /// Returns the word the `origin` log field carries.
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
    /// The rendition name, as the catalog has it.
    name: String,
    /// The rate the rung's `@<fps>` asked for.
    asked: u32,
}

/// One rung of `--renditions`, as it was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rung {
    /// The catalog name for this rendition.
    name: String,
    /// The encoded size, or `None` for the source's own resolution.
    size: Option<Size>,
    /// What the rung's `@<fps>` asked for, if it carried one.
    framerate: Option<u32>,
}

/// The simulcast ladder `--renditions` describes, and the capture frame rate it
/// settles.
#[derive(Debug)]
pub struct Ladder {
    /// The rungs, ready for `set_renditions`.
    pub renditions: Vec<VideoRendition>,
    /// The rate the capture backend is asked for.
    pub framerate: CaptureFramerate,
    /// The rungs whose `@<fps>` is not the rate the ladder is captured at, kept
    /// so [`report`](Self::report) can name them.
    unmet: Vec<UnmetRate>,
}

impl Ladder {
    /// Logs the capture frame rate, where it came from, and every rung that
    /// asked for another one.
    ///
    /// Call this once the source is known to exist: `--video none` publishes no
    /// pictures and has no capture rate to report.
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

/// The ladder and capture frame rate `args` describe for a source of kind
/// `spec`.
///
/// # Errors
///
/// Fails if a rung is not `[<name>:]<geometry>[@<fps>]`, or if `--fps` or an
/// `@<fps>` suffix names a rate no capture backend accepts.
pub fn ladder(spec: &VideoSourceSpec, args: &CaptureArgs) -> Result<Ladder> {
    let rungs = rungs(args)?;
    let framerate = capture_framerate(spec, args, &rungs)?;
    Ok(Ladder {
        renditions: renditions(args, &rungs),
        unmet: unmet_rates(&rungs, framerate),
        framerate,
    })
}

/// Parses every rung of `--renditions`, or the single unscaled rendition the
/// flag's absence stands for.
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
    // The rate comes off first so the rest of the rung parses exactly as it did
    // before the suffix existed. Splitting on the first `@` rather than the
    // last means `a@b@60` fails on `b@60` instead of quietly accepting `a@b` as
    // a rendition name.
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
        // A bare rung is either a size, which names itself, or a name standing
        // for the source's own resolution.
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

/// The rate the capture is asked for, and what settled it.
fn capture_framerate(
    spec: &VideoSourceSpec,
    args: &CaptureArgs,
    rungs: &[Rung],
) -> Result<CaptureFramerate> {
    let asked = rungs.iter().filter_map(|rung| rung.framerate).max();
    let flag = args.fps.map(flag_framerate).transpose()?;
    let framerate = match (flag, asked) {
        // `--fps` is the capture rate outright, so it caps the ladder: a rung
        // asking for more gets the rate the capture actually runs at.
        (Some(fps), Some(asked)) if asked > fps => CaptureFramerate::Requested {
            fps,
            origin: FramerateOrigin::Capped,
        },
        (Some(fps), _) => CaptureFramerate::Requested {
            fps,
            origin: FramerateOrigin::Flag,
        },
        // One capture feeds every rung, so the rung wanting the most frames
        // decides what all of them are fed.
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

/// Whether the backend behind `spec` acts on a requested frame rate.
///
/// `moq_video`'s AVFoundation camera backend does not: it warns that width,
/// height and framerate are ignored and opens the device on its own mode.
/// Requesting the default rate there would add that warning to every macOS
/// camera publish in exchange for nothing, so the default is withheld and the
/// device's own rate stands. An explicit `--fps` still goes through, because
/// somebody who typed it should see what the backend makes of it.
fn takes_framerate(spec: &VideoSourceSpec) -> bool {
    !(cfg!(target_os = "macos") && matches!(spec, VideoSourceSpec::Camera(_)))
}

/// The rungs whose `@<fps>` is not the rate the ladder is captured at.
fn unmet_rates(rungs: &[Rung], framerate: CaptureFramerate) -> Vec<UnmetRate> {
    // No request means no rung named a rate either, since a rung that did would
    // have become one.
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

/// Turns parsed rungs into the ladder `set_renditions` takes.
fn renditions(args: &CaptureArgs, rungs: &[Rung]) -> Vec<VideoRendition> {
    let codec = args.codec.into();
    let kind = video::encode::Kind::from(args.encoder);

    // `--bitrate` names the top rung, so the ladder is scaled against whichever
    // rung is largest. A rung with no explicit size encodes at the source's
    // resolution, which is at least as large as any of the others, so an
    // unsized rung means there is nothing to scale against.
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
            let mut rendition = VideoRendition::new(rung.name.clone())
                .with_codec(codec)
                .with_kind(kind.clone());
            if let Some(size) = rung.size {
                rendition = rendition.with_size(size);
            }
            if args.keyframe_interval > 0.0 {
                rendition = rendition
                    .with_keyframe_interval(Duration::from_secs_f64(args.keyframe_interval));
            }
            if let Some(bitrate) = args.bitrate {
                // Scale by pixel count against the largest rung, so a ladder
                // does not advertise the same bitrate at every size. A
                // subscriber compares its estimate against the rendition's
                // bitrate, and identical figures make the rungs
                // indistinguishable to it.
                rendition = rendition.with_bitrate(scaled_bitrate(bitrate, rung.size, largest));
            }
            rendition
        })
        .collect()
}

/// Shares `bitrate` across a ladder in proportion to pixel count.
///
/// `bitrate` is the figure for the largest rung. A rung at a quarter of the
/// pixels gets a quarter of it, floored so a very small rung is still given
/// something an encoder can work with.
fn scaled_bitrate(bitrate: u64, size: Option<Size>, largest: Option<Size>) -> u64 {
    /// Below this a rung is unusable however small it is.
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
/// Both dimensions are rounded up to the next even number: I420 chroma is
/// subsampled 2x2, so every stage of the pipeline rejects an odd one. The
/// `<height>p` shorthand assumes 16:9.
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

    /// A source kind whose backend takes a requested frame rate on every
    /// platform, so the tests below read the same on macOS as elsewhere.
    const TEST_SOURCE: VideoSourceSpec =
        VideoSourceSpec::Test(crate::source_spec::TestPattern::Timing);

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("720p"), Some(Size::new(1280, 720)));
        assert_eq!(parse_size("1080p"), Some(Size::new(1920, 1080)));
        assert_eq!(parse_size("480p"), Some(Size::new(854, 480)));
        assert_eq!(parse_size("640x360"), Some(Size::new(640, 360)));
        // I420 chroma is subsampled 2x2, so an odd dimension is rounded up
        // rather than passed to an encoder that will reject it.
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

        // The top rung keeps the figure it was given.
        let top = scaled_bitrate(
            4_000_000,
            Some(Size::new(1280, 720)),
            Some(Size::new(1280, 720)),
        );
        assert_eq!(top, 4_000_000);

        // A tiny rung still gets something an encoder can work with.
        let tiny = scaled_bitrate(
            4_000_000,
            Some(Size::new(16, 16)),
            Some(Size::new(1920, 1080)),
        );
        assert_eq!(tiny, 64_000);

        // Nothing to scale against: the figure passes through.
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
        // A rung with no geometry is a name, and takes a rate just the same.
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
        // The first `@` splits, so a second one lands in the rate and fails
        // there rather than becoming part of a rendition name.
        assert!(rung("a@b@60").is_err());
        // A rate needs something to attach to.
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
        // The slower rung is fed the same pictures as the fast one, and says so.
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
        assert_eq!(ladder.renditions.len(), 1);
        assert_eq!(ladder.renditions[0].name, SINGLE_RENDITION);
        assert_eq!(ladder.renditions[0].size, None);
    }

    #[test]
    fn a_rate_does_not_reach_the_rendition_the_encoder_is_built_from() {
        // Every rung of a ladder sees the same pictures, so `@<fps>` describes
        // the capture and leaves the rendition alone; the size still lands.
        let ladder = ladder(&TEST_SOURCE, &args(&["high:1280x720@60"], None)).unwrap();
        assert_eq!(ladder.renditions.len(), 1);
        assert_eq!(ladder.renditions[0].name, "high");
        assert_eq!(ladder.renditions[0].size, Some(Size::new(1280, 720)));
    }
}
