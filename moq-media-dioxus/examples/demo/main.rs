//! Standalone media demo using dioxus-native.
//!
//! Captures from camera or screen, optionally encodes and decodes, and renders
//! via wgpu — no network required.
//!
//! ```sh
//! cargo run -p moq-media-dioxus --example demo --features capture-screen
//! ```

use std::sync::{Arc, Mutex};

use dioxus_native::prelude::*;
#[cfg(feature = "capture-camera")]
use moq_media::capture::CameraCapturer;
#[cfg(feature = "capture-screen")]
use moq_media::capture::ScreenCapturer;
use moq_media::{
    codec::{DynamicVideoDecoder, VideoCodec},
    format::{DecodeConfig, DecoderBackend},
    pipeline::{VideoDecoderPipeline, VideoEncoderPipeline},
    subscribe::VideoTrack,
    traits::{VideoEncoder, VideoSource},
    transport::media_pipe,
};
use moq_media_dioxus::use_video_renderer;
use strum::VariantArray;

static STYLES: &str = include_str!("styles.css");

fn main() {
    tracing_subscriber::fmt::init();
    dioxus_native::launch(app);
}

// ---------------------------------------------------------------------------
// Dropdown — custom select (blitz doesn't support <select>)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Props)]
struct DropdownProps {
    label: &'static str,
    options: Vec<String>,
    selected: usize,
    onchange: EventHandler<usize>,
}

#[component]
fn Dropdown(props: DropdownProps) -> Element {
    let mut open = use_signal(|| false);

    let selected_label = props
        .options
        .get(props.selected)
        .cloned()
        .unwrap_or_default();

    rsx!(
        label { {props.label} }
        div { class: "dropdown",
            div {
                class: "dropdown-selected",
                onclick: move |_| open.set(!open()),
                span { {selected_label} }
                span { class: "dropdown-arrow", if open() { "\u{25B2}" } else { "\u{25BC}" } }
            }
            if open() {
                // Transparent overlay to close dropdown on outside click.
                div {
                    class: "dropdown-overlay",
                    onclick: move |_| open.set(false),
                }
                div { class: "dropdown-menu",
                    for (i, option) in props.options.iter().enumerate() {
                        div {
                            class: if i == props.selected { "dropdown-item active" } else { "dropdown-item" },
                            onclick: move |_| {
                                props.onchange.call(i);
                                open.set(false);
                            },
                            {option.clone()}
                        }
                    }
                }
            }
        }
    )
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
    let mut source = use_signal(|| 0usize);
    let mut mode = use_signal(|| 0usize);
    let mut codec_idx = use_signal(|| 0usize);
    let mut backend = use_signal(|| 0usize);
    let mut status = use_signal(|| String::from("idle"));

    // Keep the encoder pipeline alive; dropping it cancels the encoder thread.
    let encoder_store: Arc<Mutex<Option<VideoEncoderPipeline>>> =
        use_hook(|| Arc::new(Mutex::new(None)));

    let (handle, paint_source_id) = use_video_renderer();

    let source_names: Vec<String> = SourceKind::VARIANTS.iter().map(|s| s.to_string()).collect();
    let mode_names: Vec<String> = PipelineMode::VARIANTS
        .iter()
        .map(|m| m.to_string())
        .collect();
    let available_codecs = VideoCodec::available();
    let codec_names: Vec<String> = available_codecs
        .iter()
        .map(|c| c.display_name().to_string())
        .collect();
    let backend_names = vec!["Auto".to_string(), "Software".to_string()];

    // Start/restart pipeline when settings change.
    use_effect(move || {
        let source_kind = SourceKind::VARIANTS[source()];
        let pipeline_mode = PipelineMode::VARIANTS[mode()];
        let codec = available_codecs[codec_idx().min(available_codecs.len() - 1)];
        let decoder_backend = match backend() {
            0 => DecoderBackend::Auto,
            _ => DecoderBackend::Software,
        };

        // Drop old encoder pipeline — its Drop cancels the encoder thread.
        encoder_store.lock().expect("poisoned").take();

        status.set("starting...".into());

        match start_pipeline(source_kind, pipeline_mode, codec, decoder_backend) {
            Ok((track, info, enc)) => {
                handle.set(track);
                *encoder_store.lock().expect("poisoned") = enc;
                status.set(info);
            }
            Err(e) => status.set(format!("error: {e}")),
        }
    });

    let is_encode_decode = mode() == 1;

    rsx!(
        style { {STYLES} }
        div { id: "controls",
            Dropdown {
                label: "Source:",
                options: source_names,
                selected: source(),
                onchange: move |i| source.set(i),
            }

            Dropdown {
                label: "Mode:",
                options: mode_names,
                selected: mode(),
                onchange: move |i| mode.set(i),
            }

            if is_encode_decode {
                Dropdown {
                    label: "Encoder:",
                    options: codec_names.clone(),
                    selected: codec_idx(),
                    onchange: move |i| codec_idx.set(i),
                }

                Dropdown {
                    label: "Decoder:",
                    options: backend_names.clone(),
                    selected: backend(),
                    onchange: move |i| backend.set(i),
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
) -> Result<(VideoTrack, String, Option<VideoEncoderPipeline>), String> {
    let source = source_kind.create()?;

    match mode {
        PipelineMode::Direct => {
            let track = VideoTrack::from_video_source(
                "direct".into(),
                Default::default(),
                source,
                DecodeConfig::default(),
            );
            Ok((track, format!("{source_kind} (direct)"), None))
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
                "dioxus-demo".to_string(),
                pipe_source,
                &config,
                &decode_config,
            )
            .map_err(|e| format!("Decoder: {e:#}"))?;
            let decoder_name = dec.handle.decoder_name().to_string();
            let track = VideoTrack::from_pipeline(dec);

            Ok((
                track,
                format!("enc: {encoder_name} dec: {decoder_name}"),
                Some(enc),
            ))
        }
    }
}
