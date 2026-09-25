//! Backend selection for `--encoder` and `--decoder`.
//!
//! Both flags take a strategy (`auto`, `hardware`, `software`) or the name of
//! one backend, from moq-video's own lists (`encode::NAMES`,
//! `decode::NAMES`). An unknown name fails at parse time. A known name that
//! this build lacks, such as `vaapi` without the `vaapi` feature, fails when
//! the encoder or decoder opens.

use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use iroh_live::media::video::{decode, encode};
use serde::{Deserialize, Deserializer};

/// A backend choice: a strategy, or one backend by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, derive_more::Display)]
pub enum Backend {
    /// Tries the hardware backends in turn, then software.
    #[default]
    #[display("auto")]
    Auto,
    /// Hardware only: fails rather than fall back to the CPU.
    #[display("hardware")]
    Hardware,
    /// Software only, which is openh264.
    #[display("software")]
    Software,
    /// Only the backend of this name.
    #[display("{_0}")]
    Named(&'static str),
}

impl Backend {
    /// Returns every decoder choice.
    pub fn decoders() -> impl Iterator<Item = Self> {
        Self::choices(decode::NAMES)
    }

    /// Returns the clap parser for `--encoder`.
    pub fn encoder_parser() -> impl TypedValueParser<Value = Self> {
        Self::parser(encode::NAMES)
    }

    /// Returns the clap parser for `--decoder`.
    pub fn decoder_parser() -> impl TypedValueParser<Value = Self> {
        Self::parser(decode::NAMES)
    }

    /// Deserializes an encoder choice, for `irl run`'s session file.
    pub fn deserialize_encoder<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value, encode::NAMES)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown encoder '{value}'")))
    }

    fn choices(names: &'static [&'static str]) -> impl Iterator<Item = Self> {
        [Self::Auto, Self::Hardware, Self::Software]
            .into_iter()
            .chain(names.iter().map(|name| Self::Named(name)))
    }

    fn parse(value: &str, names: &'static [&'static str]) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "hardware" | "hw" => Some(Self::Hardware),
            "software" | "sw" => Some(Self::Software),
            other => names
                .iter()
                .find(|name| **name == other)
                .map(|name| Self::Named(name)),
        }
    }

    fn parser(names: &'static [&'static str]) -> impl TypedValueParser<Value = Self> {
        let values = Self::choices(names).map(|choice| match choice {
            Self::Hardware => PossibleValue::new("hardware").alias("hw"),
            Self::Software => PossibleValue::new("software").alias("sw"),
            Self::Auto => PossibleValue::new("auto"),
            Self::Named(name) => PossibleValue::new(name),
        });
        PossibleValuesParser::new(values)
            .map(move |value| Self::parse(&value, names).expect("clap checked the value"))
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
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Flags {
        #[arg(long, value_parser = Backend::encoder_parser(), default_value = "auto")]
        encoder: Backend,
        #[arg(long, value_parser = Backend::decoder_parser(), default_value = "auto")]
        decoder: Backend,
    }

    fn parse(args: &[&str]) -> Result<Flags, clap::Error> {
        Flags::try_parse_from(std::iter::once("irl").chain(args.iter().copied()))
    }

    #[test]
    fn strategies_and_backend_names_parse() {
        let flags = parse(&["--encoder", "hw", "--decoder", "nvdec"]).expect("valid choices");
        assert_eq!(flags.encoder, Backend::Hardware);
        assert_eq!(flags.decoder, Backend::Named("nvdec"));
        assert_eq!(parse(&[]).expect("defaults").encoder, Backend::Auto);
        assert_eq!(
            encode::Kind::from(Backend::Named("vaapi")),
            encode::Kind::Named("vaapi".to_string())
        );
    }

    #[test]
    fn a_name_no_backend_answers_to_is_rejected() {
        assert!(parse(&["--encoder", "nvdec"]).is_err());
        assert!(parse(&["--encoder", "x264"]).is_err());
        assert!(parse(&["--decoder", "nvenc"]).is_err());
    }

    #[test]
    fn every_choice_parses_back_from_its_name() {
        for choice in Backend::choices(encode::NAMES) {
            assert_eq!(
                Backend::parse(&choice.to_string(), encode::NAMES),
                Some(choice)
            );
        }
        for choice in Backend::decoders() {
            assert_eq!(
                Backend::parse(&choice.to_string(), decode::NAMES),
                Some(choice)
            );
        }
    }
}
