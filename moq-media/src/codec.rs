// TODO: remove after step 12 wires consumers to this module
#![allow(
    dead_code,
    unused_imports,
    reason = "codec module not consumed until step 12"
)]

pub(crate) mod audio;
mod resample;
pub(crate) mod video;
