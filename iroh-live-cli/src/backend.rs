//! The clap parsers and serde form of `--encoder` and `--decoder`.
//!
//! The choice itself is [`Backend`], which parses a strategy or a moq-video
//! backend name. Here clap learns the names, so `--help` lists them and a
//! wrong one fails while the arguments parse.

use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
pub(crate) use iroh_live::media::Backend;
use iroh_live::media::UnknownBackend;
use serde::{Deserialize, Deserializer};

/// Returns the clap parser for `--encoder`.
pub(crate) fn encoder_parser() -> impl TypedValueParser<Value = Backend> {
    parser(Backend::encoders(), Backend::parse_encoder)
}

/// Returns the clap parser for `--decoder`.
pub(crate) fn decoder_parser() -> impl TypedValueParser<Value = Backend> {
    parser(Backend::decoders(), Backend::parse_decoder)
}

/// Deserializes an encoder choice from an `irl run` session file.
pub(crate) fn deserialize_encoder<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Backend, D::Error> {
    let value = String::deserialize(deserializer)?;
    Backend::parse_encoder(&value).map_err(serde::de::Error::custom)
}

/// Returns a clap parser that accepts exactly `choices`, each by name and alias.
fn parser(
    choices: impl Iterator<Item = Backend>,
    parse: fn(&str) -> Result<Backend, UnknownBackend>,
) -> impl TypedValueParser<Value = Backend> {
    let values = choices.map(|choice| {
        let value = PossibleValue::new(choice.name());
        match choice.alias() {
            Some(alias) => value.alias(alias),
            None => value,
        }
    });
    PossibleValuesParser::new(values)
        .map(move |value| parse(&value).expect("clap checked the value"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use iroh_live::media::video::encode;

    use super::*;

    #[derive(Parser)]
    struct Flags {
        #[arg(long, value_parser = encoder_parser(), default_value = "auto")]
        encoder: Backend,
        #[arg(long, value_parser = decoder_parser(), default_value = "auto")]
        decoder: Backend,
    }

    /// Parses `args` as if typed after `irl`.
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
}
