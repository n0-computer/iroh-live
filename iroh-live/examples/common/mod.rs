#![allow(
    clippy::mod_module_files,
    reason = "examples/common/ needs mod.rs for cargo example discovery"
)]
#[allow(unreachable_pub, unused, reason = "example helper module")]
pub mod import;

#[macro_export]
macro_rules! clap_enum_variants {
    ($e: ty) => {{
        use clap::builder::TypedValueParser;
        use strum::VariantNames;
        clap::builder::PossibleValuesParser::new(<$e>::VARIANTS).map(|s| s.parse::<$e>().unwrap())
    }};
}
