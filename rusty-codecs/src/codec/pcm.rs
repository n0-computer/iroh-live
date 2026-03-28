/// PCM (raw samples) audio codec — zero-compression passthrough.
///
/// On local or low-latency networks, encoding overhead is wasted when
/// bandwidth is plentiful. PCM avoids the ~20ms framing latency and CPU
/// cost of Opus by shipping raw interleaved f32 samples directly. The
/// encoder wraps 20ms chunks into [`EncodedFrame`]s; the decoder unwraps
/// them back into PCM samples.
///
/// Wire format: raw little-endian f32 samples, interleaved, 20ms per
/// packet at the configured sample rate and channel count.
pub(crate) mod decoder;
pub(crate) mod encoder;

pub use decoder::PcmAudioDecoder;
pub use encoder::PcmEncoder;
