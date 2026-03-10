//! Standalone media viewer using dioxus-native.
//!
//! Captures from camera or screen, optionally encodes and decodes, and renders
//! via wgpu — no network required.
//!
//! ```sh
//! cargo run -p moq-media-dioxus --example viewer
//! ```

use std::sync::mpsc;

use dioxus_native::prelude::*;
use dioxus_native::use_wgpu;
#[cfg(feature = "capture-camera")]
use moq_media::capture::CameraCapturer;
#[cfg(feature = "capture-screen")]
use moq_media::capture::ScreenCapturer;
use moq_media::codec::{DynamicVideoDecoder, VideoCodec};
use moq_media::format::{DecodeConfig, DecodedVideoFrame, DecoderBackend};
use moq_media::pipeline::{VideoDecoderPipeline, VideoEncoderPipeline};
use moq_media::subscribe::WatchTrack;
use moq_media::traits::{VideoEncoder, VideoSource};
use moq_media::transport::media_pipe;
use moq_media_dioxus::DioxusVideoRenderer;
use strum::VariantArray;
use tokio_util::sync::CancellationToken;

static STYLES: &str = include_str!("styles.css");

fn main() {
    tracing_subscriber::fmt::init();
    dioxus_native::launch(app);
}

// ---------------------------------------------------------------------------
// Source selection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum SourceKind {
    #[cfg(feature = "capture-camera")]
    Camera,
    #[cfg(feature = "capture-screen")]
    Screen,
}

impl SourceKind {
    fn create(self) -> Result<Box<dyn VideoSource>, String> {
        match self {
            #[cfg(feature = "capture-camera")]
            Self::Camera => CameraCapturer::new()
                .map(|c| Box::new(c) as Box<dyn VideoSource>)
                .map_err(|e| format!("{e:#}")),
            #[cfg(feature = "capture-screen")]
            Self::Screen => ScreenCapturer::new()
                .map(|c| Box::new(c) as Box<dyn VideoSource>)
                .map_err(|e| format!("{e:#}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, strum::Display, strum::VariantArray)]
enum PipelineMode {
    Direct,
    #[strum(serialize = "Encode/Decode")]
    EncodeDecode,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

fn app() -> Element {
    let mut source = use_signal(|| SourceKind::VARIANTS[0]);
    let mut mode = use_signal(|| PipelineMode::Direct);
    let mut codec = use_signal(|| VideoCodec::best_available());
    let mut backend = use_signal(|| DecoderBackend::Auto);
    let mut status = use_signal(|| String::from("idle"));

    let renderer = DioxusVideoRenderer::new();
    let frame_tx = renderer.frame_sender();
    let paint_source_id = use_wgpu(move || renderer);

    // Start/restart pipeline when settings change.
    use_effect(move || {
        let source = source();
        let mode = mode();
        let codec = codec();
        let backend = backend();
        let frame_tx = frame_tx.clone();

        status.set("starting...".into());

        match start_pipeline(source, mode, codec, backend, frame_tx) {
            Ok(info) => status.set(info),
            Err(e) => status.set(format!("error: {e}")),
        }
    });

    let is_encode_decode = mode() == PipelineMode::EncodeDecode;

    rsx!(
        style { {STYLES} }
        div { id: "controls",
            label { "Source:" }
            select {
                onchange: move |evt| {
                    if let Some(s) = SourceKind::VARIANTS.iter().find(|s| s.to_string() == evt.value()) {
                        source.set(*s);
                    }
                },
                for s in SourceKind::VARIANTS {
                    option {
                        value: s.to_string(),
                        selected: *s == source(),
                        {s.to_string()}
                    }
                }
            }

            label { "Mode:" }
            select {
                onchange: move |evt| {
                    if let Some(m) = PipelineMode::VARIANTS.iter().find(|m| m.to_string() == evt.value()) {
                        mode.set(*m);
                    }
                },
                for m in PipelineMode::VARIANTS {
                    option {
                        value: m.to_string(),
                        selected: *m == mode(),
                        {m.to_string()}
                    }
                }
            }

            if is_encode_decode {
                label { "Encoder:" }
                select {
                    onchange: move |evt| {
                        for c in VideoCodec::available() {
                            if c.display_name() == evt.value() {
                                codec.set(c);
                                break;
                            }
                        }
                    },
                    for c in VideoCodec::available() {
                        option {
                            value: c.display_name(),
                            selected: c == codec(),
                            {c.display_name()}
                        }
                    }
                }

                label { "Decoder:" }
                select {
                    onchange: move |evt| {
                        backend.set(match evt.value().as_str() {
                            "Software" => DecoderBackend::Software,
                            _ => DecoderBackend::Auto,
                        });
                    },
                    option { value: "Auto", selected: backend() == DecoderBackend::Auto, "Auto (HW > SW)" }
                    option { value: "Software", selected: backend() == DecoderBackend::Software, "Software" }
                }
            }

            span { id: "status", {status()} }
        }
        div { id: "canvas-container",
            canvas { id: "video-canvas", "src": paint_source_id }
        }
    )
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

fn start_pipeline(
    source_kind: SourceKind,
    mode: PipelineMode,
    codec: VideoCodec,
    backend: DecoderBackend,
    frame_tx: mpsc::Sender<DecodedVideoFrame>,
) -> Result<String, String> {
    let source = source_kind.create()?;
    let shutdown = CancellationToken::new();

    let (track, info) = match mode {
        PipelineMode::Direct => {
            let track = WatchTrack::from_video_source(
                "direct".into(),
                shutdown.clone(),
                source,
                DecodeConfig::default(),
            );
            (track, format!("{source_kind} (direct)"))
        }
        PipelineMode::EncodeDecode => {
            let encoder = codec
                .create_encoder_from_source(&*source)
                .map_err(|e| format!("Encoder: {e:#}"))?;
            let encoder_name = encoder.name().to_string();
            let config = encoder.config();
            let (sink, pipe_source) = media_pipe(32);
            let enc = VideoEncoderPipeline::new(source, encoder, sink);

            let decode_config = DecodeConfig {
                backend,
                ..Default::default()
            };
            let dec = VideoDecoderPipeline::new::<DynamicVideoDecoder>(
                "dioxus-viewer".to_string(),
                pipe_source,
                &config,
                &decode_config,
            )
            .map_err(|e| format!("Decoder: {e:#}"))?;
            let decoder_name = dec.handle.decoder_name().to_string();
            let track = WatchTrack::from_pipeline(dec);

            // Keep encoder alive in a background thread.
            std::thread::spawn(move || {
                let _enc = enc;
                let _shutdown = shutdown;
                std::thread::park();
            });

            (track, format!("enc: {encoder_name} dec: {decoder_name}"))
        }
    };

    // Forward frames to the renderer.
    std::thread::spawn(move || {
        let mut track = track;
        loop {
            if let Some(frame) = track.current_frame() {
                if frame_tx.send(frame).is_err() {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    });

    Ok(info)
}
