//! A choice of video encoder or decoder backend, as a flag or a config file names it.
//!
//! A choice is a strategy (`auto`, `hardware`, `software`) or one backend by
//! name, from moq-video's [`encode::NAMES`] or [`decode::NAMES`]. An unknown
//! name fails to parse. A known name this build lacks, such as `vaapi`
//! without moq-video's `vaapi` feature, fails when the encoder or decoder
//! opens.

use crate::video::{decode, encode};

/// A choice of encoder or decoder backend: a strategy, or one backend by name.
///
/// Converts into moq-video's [`encode::Kind`] and [`decode::Kind`]. It
/// displays as the name it parses from.
///
/// # Examples
///
/// ```
/// use moq_ext_sources::Backend;
///
/// assert_eq!(Backend::parse_encoder("hw"), Ok(Backend::Hardware));
/// let kind: moq_video::encode::Kind = Backend::Software.into();
/// assert_eq!(kind, moq_video::encode::Kind::Software);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, derive_more::Display)]
#[display("{}", self.name())]
pub enum Backend {
    /// Tries the hardware backends in turn, then software.
    #[default]
    Auto,
    /// Hardware only, with no fallback to software.
    Hardware,
    /// Software only.
    Software,
    /// Only the backend of this name, one of moq-video's backend names.
    Named(&'static str),
}

/// An error from parsing a backend name that is no strategy and no known backend.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
#[display("unknown backend '{_0}': expected auto, hardware, software, or one of [{}]", _1.join(", "))]
pub struct UnknownBackend(String, &'static [&'static str]);

impl std::error::Error for UnknownBackend {}

impl Backend {
    /// Returns every encoder choice: the strategies, then each encoder backend by name.
    pub fn encoders() -> impl Iterator<Item = Self> {
        Self::choices(encode::NAMES)
    }

    /// Returns every decoder choice: the strategies, then each decoder backend by name.
    pub fn decoders() -> impl Iterator<Item = Self> {
        Self::choices(decode::NAMES)
    }

    /// Parses an encoder choice, ignoring case.
    ///
    /// Takes `auto`, `hardware` or `hw`, `software` or `sw`, or a name from
    /// [`encode::NAMES`].
    ///
    /// # Errors
    ///
    /// Fails for any other value, with a message that lists the names.
    pub fn parse_encoder(value: &str) -> Result<Self, UnknownBackend> {
        Self::parse(value, encode::NAMES)
    }

    /// Parses a decoder choice, ignoring case.
    ///
    /// Takes the strategies as [`parse_encoder`](Self::parse_encoder) does, or
    /// a name from [`decode::NAMES`].
    ///
    /// # Errors
    ///
    /// Fails for any other value, with a message that lists the names.
    pub fn parse_decoder(value: &str) -> Result<Self, UnknownBackend> {
        Self::parse(value, decode::NAMES)
    }

    /// Returns the name the choice parses from and displays as.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Named(name) => name,
        }
    }

    /// Returns the short alias the choice also parses from, such as `hw`.
    #[must_use]
    pub fn alias(self) -> Option<&'static str> {
        match self {
            Self::Hardware => Some("hw"),
            Self::Software => Some("sw"),
            Self::Auto | Self::Named(_) => None,
        }
    }

    /// Returns the strategies followed by one choice per name in `names`.
    fn choices(names: &'static [&'static str]) -> impl Iterator<Item = Self> {
        [Self::Auto, Self::Hardware, Self::Software]
            .into_iter()
            .chain(names.iter().map(|name| Self::Named(name)))
    }

    /// Parses a strategy, its short alias, or a name in `names`, ignoring case.
    fn parse(value: &str, names: &'static [&'static str]) -> Result<Self, UnknownBackend> {
        match value.to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "hardware" | "hw" => Ok(Self::Hardware),
            "software" | "sw" => Ok(Self::Software),
            other => names
                .iter()
                .find(|name| **name == other)
                .map(|name| Self::Named(name))
                .ok_or_else(|| UnknownBackend(value.to_string(), names)),
        }
    }
}

impl From<Backend> for encode::Kind {
    fn from(backend: Backend) -> Self {
        match backend {
            Backend::Auto => Self::Auto,
            Backend::Hardware => Self::Hardware,
            Backend::Software => Self::Software,
            Backend::Named(name) => Self::Named(name.to_string()),
        }
    }
}

impl From<Backend> for decode::Kind {
    fn from(backend: Backend) -> Self {
        match backend {
            Backend::Auto => Self::Auto,
            Backend::Hardware => Self::Hardware,
            Backend::Software => Self::Software,
            Backend::Named(name) => Self::Named(name.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategies_aliases_and_names_parse() {
        assert_eq!(Backend::parse_encoder("HW"), Ok(Backend::Hardware));
        assert_eq!(Backend::parse_decoder("sw"), Ok(Backend::Software));
        assert_eq!(Backend::parse_decoder("nvdec"), Ok(Backend::Named("nvdec")));
        assert_eq!(
            encode::Kind::from(Backend::Named("vaapi")),
            encode::Kind::Named("vaapi".to_string())
        );
    }

    /// An encoder name is no decoder, and the error lists what would parse.
    #[test]
    fn a_name_of_the_other_kind_is_rejected() {
        assert!(Backend::parse_encoder("nvdec").is_err());
        assert!(Backend::parse_decoder("nvenc").is_err());
        let err = Backend::parse_encoder("x264").unwrap_err();
        assert!(err.to_string().contains("openh264"), "{err}");
    }

    #[test]
    fn every_choice_parses_back_from_its_name() {
        for choice in Backend::encoders() {
            assert_eq!(Backend::parse_encoder(&choice.to_string()), Ok(choice));
        }
        for choice in Backend::decoders() {
            assert_eq!(Backend::parse_decoder(&choice.to_string()), Ok(choice));
            if let Some(alias) = choice.alias() {
                assert_eq!(Backend::parse_decoder(alias), Ok(choice));
            }
        }
    }
}
