//! Backend selection for `--encoder` and `--decoder`.
//!
//! Both flags name either a strategy (try everything, hardware only, software
//! only) or one backend and nothing else. The backend names are moq-video's
//! own, spelled out here because upstream keeps its `NAME` constants
//! crate-private: one per file under `moq-video/src/encode/backend/` and
//! `moq-video/src/decode/backend/`. When upstream gains a backend, this is the
//! list that has to learn about it.
//!
//! Spelling them out is also what makes an unknown name a parse error. Passed
//! through, it reaches moq-video as `Kind::Named` that no candidate answers to,
//! and comes back as "no encoder available" naming nothing, long after the
//! capture devices are open.
//!
//! A name that exists but is not in this build, `vaapi` without the `vaapi`
//! feature or `videotoolbox` anywhere but macOS, is a different failure and
//! still comes from the media stack. Which backends a build has is a
//! compile-time question the flag cannot answer.

use std::fmt;

use clap::ValueEnum;
use iroh_live::media::video::{decode, encode};
use serde::Deserialize;

/// The video encoder `--encoder` selects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderArg {
    /// Try the platform's hardware encoders in turn, then software.
    #[default]
    Auto,
    /// Hardware only: fail rather than fall back to the CPU.
    #[value(alias = "hw")]
    #[serde(alias = "hw")]
    Hardware,
    /// Software only, which is openh264.
    #[value(alias = "sw")]
    #[serde(alias = "sw")]
    Software,
    /// Apple VideoToolbox, on macOS.
    Videotoolbox,
    /// Media Foundation, on Windows.
    Mediafoundation,
    /// MediaCodec, on Android.
    Mediacodec,
    /// NVENC, on an NVIDIA GPU.
    Nvenc,
    /// VA-API, on an Intel or AMD GPU under Linux.
    Vaapi,
    /// A V4L2 memory-to-memory codec node, on an ARM SoC.
    V4l2,
    /// openh264, the software encoder, by name.
    Openh264,
}

/// The video decoder `--decoder` selects.
///
/// The same shape as [`EncoderArg`] over a different set of backends: NVIDIA
/// decodes through NVDEC rather than NVENC, and the rest happen to be named the
/// same on both sides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecoderArg {
    /// Try the platform's hardware decoders in turn, then software.
    #[default]
    Auto,
    /// Hardware only: fail rather than fall back to the CPU.
    #[value(alias = "hw")]
    #[serde(alias = "hw")]
    Hardware,
    /// Software only, which is openh264.
    #[value(alias = "sw")]
    #[serde(alias = "sw")]
    Software,
    /// Apple VideoToolbox, on macOS.
    Videotoolbox,
    /// Media Foundation, on Windows.
    Mediafoundation,
    /// MediaCodec, on Android.
    Mediacodec,
    /// NVDEC, on an NVIDIA GPU.
    Nvdec,
    /// VA-API, on an Intel or AMD GPU under Linux.
    Vaapi,
    /// A V4L2 memory-to-memory codec node, on an ARM SoC.
    V4l2,
    /// openh264, the software decoder, by name.
    Openh264,
}

impl fmt::Display for EncoderArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.to_possible_value().expect(SPELLED_OUT);
        f.write_str(value.get_name())
    }
}

impl fmt::Display for DecoderArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.to_possible_value().expect(SPELLED_OUT);
        f.write_str(value.get_name())
    }
}

/// Why a variant always has a name: none of them is `#[value(skip)]`, so clap
/// generated one for every one.
const SPELLED_OUT: &str = "every variant of a backend flag has a name";

impl From<EncoderArg> for encode::Kind {
    fn from(arg: EncoderArg) -> Self {
        match arg {
            EncoderArg::Auto => Self::Auto,
            EncoderArg::Hardware => Self::Hardware,
            EncoderArg::Software => Self::Software,
            named => Self::Named(named.to_string()),
        }
    }
}

impl From<DecoderArg> for decode::Kind {
    fn from(arg: DecoderArg) -> Self {
        match arg {
            DecoderArg::Auto => Self::Auto,
            DecoderArg::Hardware => Self::Hardware,
            DecoderArg::Software => Self::Software,
            named => Self::Named(named.to_string()),
        }
    }
}

impl DecoderArg {
    /// The selection `kind` stands for, or [`Auto`](Self::Auto) for anything
    /// this flag cannot spell.
    ///
    /// Reads back what [`From`] wrote, so a window can seed its picker from the
    /// policy the broadcast is already playing under rather than tracking the
    /// choice alongside it. `moq_video::decode::Kind` is `#[non_exhaustive]` and
    /// its `Named` carries a free-form string, so not every value it can hold
    /// has a variant here.
    pub fn from_kind(kind: &decode::Kind) -> Self {
        match kind {
            decode::Kind::Hardware => Self::Hardware,
            decode::Kind::Software => Self::Software,
            decode::Kind::Named(name) => Self::from_str(name, true).unwrap_or(Self::Auto),
            _ => Self::Auto,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_match_upstream() {
        // The names moq-video's backends are registered under. Change these
        // only alongside `moq-video/src/{encode,decode}/backend/`.
        let expected = [
            (EncoderArg::Auto, "auto"),
            (EncoderArg::Hardware, "hardware"),
            (EncoderArg::Software, "software"),
            (EncoderArg::Videotoolbox, "videotoolbox"),
            (EncoderArg::Mediafoundation, "mediafoundation"),
            (EncoderArg::Mediacodec, "mediacodec"),
            (EncoderArg::Nvenc, "nvenc"),
            (EncoderArg::Vaapi, "vaapi"),
            (EncoderArg::V4l2, "v4l2"),
            (EncoderArg::Openh264, "openh264"),
        ];
        for (arg, name) in expected {
            assert_eq!(arg.to_string(), name);
            assert_eq!(EncoderArg::from_str(name, true), Ok(arg));
        }

        let expected = [
            (DecoderArg::Auto, "auto"),
            (DecoderArg::Hardware, "hardware"),
            (DecoderArg::Software, "software"),
            (DecoderArg::Videotoolbox, "videotoolbox"),
            (DecoderArg::Mediafoundation, "mediafoundation"),
            (DecoderArg::Mediacodec, "mediacodec"),
            (DecoderArg::Nvdec, "nvdec"),
            (DecoderArg::Vaapi, "vaapi"),
            (DecoderArg::V4l2, "v4l2"),
            (DecoderArg::Openh264, "openh264"),
        ];
        for (arg, name) in expected {
            assert_eq!(arg.to_string(), name);
            assert_eq!(DecoderArg::from_str(name, true), Ok(arg));
        }
    }

    #[test]
    fn a_name_no_backend_answers_to_is_rejected() {
        assert!(EncoderArg::from_str("nvdec", true).is_err());
        assert!(EncoderArg::from_str("x264", true).is_err());
        assert!(DecoderArg::from_str("nvenc", true).is_err());
        assert!(DecoderArg::from_str("", true).is_err());
    }

    #[test]
    fn the_short_spellings_are_accepted() {
        assert_eq!(EncoderArg::from_str("hw", true), Ok(EncoderArg::Hardware));
        assert_eq!(EncoderArg::from_str("sw", true), Ok(EncoderArg::Software));
        assert_eq!(DecoderArg::from_str("HW", true), Ok(DecoderArg::Hardware));
        assert_eq!(DecoderArg::from_str("Sw", true), Ok(DecoderArg::Software));
    }

    #[test]
    fn a_strategy_maps_to_a_strategy_and_a_backend_to_its_name() {
        assert_eq!(encode::Kind::from(EncoderArg::Auto), encode::Kind::Auto);
        assert_eq!(
            encode::Kind::from(EncoderArg::Hardware),
            encode::Kind::Hardware
        );
        assert_eq!(
            encode::Kind::from(EncoderArg::Vaapi),
            encode::Kind::Named("vaapi".to_string())
        );
        assert_eq!(decode::Kind::from(DecoderArg::Auto), decode::Kind::Auto);
        assert_eq!(
            decode::Kind::from(DecoderArg::Software),
            decode::Kind::Software
        );
        assert_eq!(
            decode::Kind::from(DecoderArg::Nvdec),
            decode::Kind::Named("nvdec".to_string())
        );
    }

    #[test]
    fn a_decoder_selection_survives_the_round_trip_through_a_policy() {
        for arg in DecoderArg::value_variants() {
            let kind = decode::Kind::from(*arg);
            assert_eq!(DecoderArg::from_kind(&kind), *arg);
        }
    }

    #[test]
    fn a_backend_the_flag_cannot_spell_reads_back_as_auto() {
        let kind = decode::Kind::Named("something-upstream-added".to_string());
        assert_eq!(DecoderArg::from_kind(&kind), DecoderArg::Auto);
    }

    /// The strategy names are ours; every other value has to be a name
    /// `moq-video` answers to, and upstream now publishes that vocabulary.
    ///
    /// The test beside this one compares the enum against string literals in
    /// this same file, which proves only that the file agrees with itself. A
    /// backend renamed upstream would pass it and then fail at the moment
    /// somebody selected the name, as `NoEncoder`. This compares against the
    /// list upstream keeps, and upstream asserts that list covers every backend
    /// it compiled, so a rename fails here and an addition fails there.
    #[test]
    fn every_backend_upstream_names_is_offered() {
        let ours: Vec<String> = EncoderArg::value_variants()
            .iter()
            .map(ToString::to_string)
            .filter(|name| !matches!(name.as_str(), "auto" | "hardware" | "software"))
            .collect();
        let mut theirs: Vec<&str> = moq_media::video::encode::NAMES.to_vec();
        theirs.sort_unstable();
        let mut ours: Vec<&str> = ours.iter().map(String::as_str).collect();
        ours.sort_unstable();
        assert_eq!(ours, theirs, "the encoder list has drifted from moq-video");

        let ours: Vec<String> = DecoderArg::value_variants()
            .iter()
            .map(ToString::to_string)
            .filter(|name| !matches!(name.as_str(), "auto" | "hardware" | "software"))
            .collect();
        let mut theirs: Vec<&str> = moq_media::video::decode::NAMES.to_vec();
        theirs.sort_unstable();
        let mut ours: Vec<&str> = ours.iter().map(String::as_str).collect();
        ours.sort_unstable();
        assert_eq!(ours, theirs, "the decoder list has drifted from moq-video");
    }
}
