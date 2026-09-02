//! `irl publish`: publish a capture device or a media file over iroh.
//!
//! Capture sources go through `moq-media`'s encode path, which fans one device
//! out to the simulcast ladder `--renditions` describes. A `file:` source takes
//! the import path instead: its tracks are republished as they already are.

use iroh_live::{Live, media::publish::LocalBroadcast};
use n0_error::Result;

use crate::{
    args::PublishArgs,
    import::FileImport,
    source,
    source_spec::VideoSourceSpec,
    transport::{self, setup_live},
};

/// Runs the `publish` command.
pub fn run(args: PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    match args.capture.video_source()? {
        VideoSourceSpec::File(path) => publish_file(&path, &args, rt),
        _ => publish_capture(&args, rt),
    }
}

/// Opens the devices, publishes them, and prints the ticket.
async fn setup_capture(args: &PublishArgs) -> Result<(Live, LocalBroadcast, String)> {
    let live = setup_live(!args.transport.no_serve).await?;
    let broadcast = live.publish(&args.transport.name)?;
    source::configure(&broadcast, &args.capture)?;
    transport::advertise(&live, &args.transport).await?;
    let ticket = transport::ticket(&live, &args.transport.name);
    Ok((live, broadcast, ticket))
}

/// Publishes capture devices, optionally alongside a preview window.
fn publish_capture(args: &PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    let (live, broadcast, ticket) = rt.block_on(setup_capture(args))?;

    if !args.preview {
        return wait_for_ctrl_c(rt, live, broadcast);
    }

    #[cfg(feature = "render")]
    {
        // eframe owns the main thread, so the runtime stays alive only for as
        // long as this guard does.
        let _guard = rt.enter();
        preview::run(live, broadcast, ticket, &args.capture.video)
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = ticket;
        drop(broadcast);
        drop(live);
        Err(n0_error::anyerr!("--preview needs the 'render' feature"))
    }
}

/// Publishes a media file, republishing its tracks without decoding them.
fn publish_file(
    path: &std::path::Path,
    args: &PublishArgs,
    rt: &tokio::runtime::Runtime,
) -> Result {
    if args.preview {
        return Err(n0_error::anyerr!(
            "--preview is not available for a file source: its tracks are \
             republished as they are, so there are no raw frames to draw"
        ));
    }

    rt.block_on(run_file(path, args))
}

/// Publishes the file and holds it open until end of input or an interrupt.
async fn run_file(path: &std::path::Path, args: &PublishArgs) -> Result {
    let live = setup_live(!args.transport.no_serve).await?;
    let producer = live.publish_raw(&args.transport.name)?;
    let import = FileImport::open(producer, path, args.format, args.transcode).await?;
    transport::advertise(&live, &args.transport).await?;

    println!("press Ctrl+C to stop");
    tokio::select! {
        result = import.run() => result?,
        _ = tokio::signal::ctrl_c() => {}
    }
    live.shutdown().await;
    Ok(())
}

/// Holds the broadcast open until the user interrupts.
fn wait_for_ctrl_c(rt: &tokio::runtime::Runtime, live: Live, broadcast: LocalBroadcast) -> Result {
    println!("press Ctrl+C to stop");
    rt.block_on(async move {
        tokio::signal::ctrl_c().await?;
        broadcast.finish();
        live.shutdown().await;
        Ok(())
    })
}

#[cfg(feature = "render")]
mod preview {
    //! The preview window: the frames on their way to the encoders, plus a
    //! source picker that swaps the capture device without restarting the
    //! broadcast.

    use std::time::Duration;

    use eframe::egui;
    use iroh_live::{
        Live,
        media::{
            publish::{LocalBroadcast, VideoRendition, VideoSource},
            video,
        },
    };
    use moq_media_egui::overlay::{DebugOverlay, StatCategory, fit_to_aspect};
    use n0_error::{Result, anyerr};
    use tracing::{info, warn};

    use crate::{source_spec::VideoSourceSpec, ui::LocalPreview};

    /// Opens the preview window and runs it until it closes.
    ///
    /// `flag` is the `--video` specifier the broadcast started with, so the
    /// picker can show what is already publishing instead of guessing.
    pub(super) fn run(live: Live, broadcast: LocalBroadcast, ticket: String, flag: &str) -> Result {
        eframe::run_native(
            "irl publish",
            crate::ui::native_options(false),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                let view =
                    LocalPreview::new(&cc.egui_ctx, "preview", cc.wgpu_render_state.as_ref());
                Ok(Box::new(PreviewApp {
                    live,
                    broadcast,
                    ticket,
                    view,
                    picker: SourcePicker::new(flag),
                    overlay: DebugOverlay::new(&[StatCategory::Capture, StatCategory::Net]),
                }))
            }),
        )
        .map_err(|err| anyerr!("eframe failed: {err:#}"))
    }

    struct PreviewApp {
        live: Live,
        broadcast: LocalBroadcast,
        ticket: String,
        view: LocalPreview,
        picker: SourcePicker,
        overlay: DebugOverlay,
    }

    impl eframe::App for PreviewApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            ctx.request_repaint_after(Duration::from_millis(16));

            self.view.update(&ctx, &self.broadcast);

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            crate::ui::top_bar(ui, &ctx, &self.ticket);

            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            let size = fit_to_aspect(available, 16.0 / 9.0);
            let image = self.view.image();
            ui.centered_and_justified(|ui| ui.add_sized(size, image));

            self.overlay
                .show_publish(ui, video_rect, self.broadcast.stats());

            crate::ui::control_panel(&ctx, "publish-controls", |ui| {
                self.picker.ui(ui, &self.broadcast);
            });
        }

        fn on_exit(&mut self) {
            info!("exit");
            crate::ui::shutdown_live_blocking(&self.live);
        }
    }

    /// A source the picker can switch to.
    ///
    /// Deliberately coarse: the combo offers the default camera, the default
    /// display, the test pattern, and nothing. Choosing a specific device is
    /// what `--video` and `irl devices` are for.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum PickedSource {
        Camera,
        Screen,
        Test,
        None,
    }

    impl PickedSource {
        const ALL: [Self; 4] = [Self::Camera, Self::Screen, Self::Test, Self::None];

        fn label(self) -> &'static str {
            match self {
                Self::Camera => "Camera",
                Self::Screen => "Screen",
                Self::Test => "Test pattern",
                Self::None => "No video",
            }
        }

        /// The entry matching a `--video` specifier, if the combo has one.
        ///
        /// A window or an application source names a device the combo cannot
        /// express, so those start unmatched and the picker shows the flag
        /// text until the user chooses something else.
        fn from_spec(spec: &VideoSourceSpec) -> Option<Self> {
            match spec {
                VideoSourceSpec::Camera(None) => Some(Self::Camera),
                VideoSourceSpec::Display(None) => Some(Self::Screen),
                VideoSourceSpec::Test => Some(Self::Test),
                VideoSourceSpec::None => Some(Self::None),
                _ => None,
            }
        }
    }

    /// The source combo.
    ///
    /// Switching is just `set_renditions` again: it replaces whatever was
    /// publishing, so there is no separate teardown step and the catalog keeps
    /// the same rendition name across the swap.
    #[derive(Debug)]
    struct SourcePicker {
        selected: Option<PickedSource>,
        /// What `--video` said, shown while nothing in the combo matches it.
        flag: String,
        error: Option<String>,
    }

    impl SourcePicker {
        fn new(flag: &str) -> Self {
            let selected = VideoSourceSpec::parse(flag)
                .ok()
                .as_ref()
                .and_then(PickedSource::from_spec);
            Self {
                selected,
                flag: flag.to_string(),
                error: None,
            }
        }

        fn ui(&mut self, ui: &mut egui::Ui, broadcast: &LocalBroadcast) {
            ui.label("Video");
            let label = match self.selected {
                Some(source) => source.label(),
                None => self.flag.as_str(),
            };
            let mut changed = false;
            egui::ComboBox::from_id_salt("preview-source")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for source in PickedSource::ALL {
                        changed |= ui
                            .selectable_value(&mut self.selected, Some(source), source.label())
                            .changed();
                    }
                });

            if changed {
                self.error = self.apply(broadcast).err().map(|err| format!("{err:#}"));
                if let Some(err) = &self.error {
                    warn!(error = %err, "source switch failed");
                }
            }
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
        }

        /// Publishes the selected source at the source's own resolution.
        fn apply(&self, broadcast: &LocalBroadcast) -> Result<()> {
            let source = match self.selected {
                None | Some(PickedSource::None) => {
                    broadcast.video().clear();
                    return Ok(());
                }
                Some(PickedSource::Camera) => {
                    VideoSource::Capture(video::capture::Config::default())
                }
                Some(PickedSource::Screen) => {
                    let mut config = video::capture::Config::default();
                    config.source = video::capture::Source::Display(None);
                    VideoSource::Capture(config)
                }
                Some(PickedSource::Test) => crate::source::default_test_pattern(),
            };
            broadcast
                .video()
                .set_renditions(source, vec![VideoRendition::new("video")])?;
            Ok(())
        }
    }
}
