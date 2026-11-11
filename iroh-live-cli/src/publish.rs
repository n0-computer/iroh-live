//! `irl publish`: publishes a capture device or a media file over iroh.
//!
//! Capture sources are encoded into the renditions `--renditions` lists. A
//! `file:` source is republished without re-encoding.

use iroh_live::{Node, layout::LiveLayout, media::LocalBroadcast, net::broadcast};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

use crate::{
    args::PublishArgs,
    import::{FileImport, FileSource},
    source,
    source_spec::VideoSourceSpec,
    transport::{self, SharedNode, bind_node},
};

/// Runs the `publish` command.
pub fn run(args: PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    match args.capture.video_source() {
        VideoSourceSpec::File { path, looping } => {
            publish_file(FileSource::new(path, looping, &args)?, &args, rt)
        }
        // `rpicam:raw` captures raw frames, so it falls through to the
        // capture path and can be previewed.
        #[cfg(all(target_os = "linux", feature = "rpicam"))]
        VideoSourceSpec::Rpicam(crate::source_spec::RpicamMode::Encoded) if args.preview => {
            Err(anyerr!(
                "--preview is not available for --video rpicam: rpicam-vid hands \
                 over H.264 it has already encoded, so there are no raw frames to \
                 draw. --video rpicam:raw captures pictures and can be previewed"
            ))
        }
        _ => {
            if args.transcode {
                warn!("ignoring --transcode: it only applies to a file: video source");
            }
            publish_capture(&args, rt)
        }
    }
}

/// Opens the devices, publishes them, and prints the ticket.
async fn setup_capture(
    args: &PublishArgs,
) -> Result<(SharedNode, LocalBroadcast, source::Opened, String)> {
    let node = bind_node(!args.transport.no_serve).await?;
    let (node, (broadcast, sources, ticket)) = transport::with_node(node, async |node| {
        let broadcast: LocalBroadcast =
            LiveLayout::default().publish(node, &args.transport.name)?;
        let sources = source::configure(&broadcast, &args.capture, None).await?;
        let ticket = transport::advertise(node, &args.transport)?;
        // `--test-source` overrides both flags, so log what was opened.
        info!(
            name = %args.transport.name,
            video = %args.capture.video_source(),
            audio = %args.capture.audio_source(),
            "publishing"
        );
        Ok((broadcast, sources, ticket))
    })
    .await?;
    Ok((node, broadcast, sources, ticket))
}

/// Publishes capture devices, optionally alongside a preview window.
fn publish_capture(args: &PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    let (node, broadcast, sources, ticket) = rt.block_on(setup_capture(args))?;

    if !args.preview {
        return wait_for_ctrl_c(rt, node, broadcast, sources);
    }

    // eframe takes the main thread. The guard keeps the runtime entered.
    let _guard = rt.enter();
    preview::run(node, broadcast, sources, ticket, args)
}

/// Publishes a media file, republishing its tracks without decoding them.
fn publish_file(source: FileSource, args: &PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    if args.preview {
        return Err(anyerr!(
            "--preview is not available for a file source: its tracks are \
             republished as they are, so there are no raw frames to draw"
        ));
    }

    rt.block_on(run_file(source, args))
}

/// Publishes the file and holds it open until end of input or an interrupt.
async fn run_file(source: FileSource, args: &PublishArgs) -> Result {
    let node = bind_node(!args.transport.no_serve).await?;
    let result = publish_import(&node, source, args).await;
    node.shutdown().await;
    result
}

/// Publishes the file on `node`. The caller shuts `node` down.
async fn publish_import(node: &Node, source: FileSource, args: &PublishArgs) -> Result {
    // The import creates its tracks once it has read the file's header, a
    // moment after the broadcast is published. A reader that asks within that
    // moment finds no catalog yet: `RemoteBroadcast` asks again until it
    // appears, while other readers may give up.
    let producer: broadcast::Producer =
        LiveLayout::default().publish(node, &args.transport.name)?;
    let import = match FileImport::open(producer.clone(), source).await {
        Ok(import) => import,
        Err(err) => {
            // Withdraws the publication.
            producer.close();
            return Err(err);
        }
    };
    transport::advertise(node, &args.transport)?;
    info!(name = %args.transport.name, "publishing a file");

    println!("press Ctrl+C to stop");
    let result = tokio::select! {
        result = import.run() => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    // The node holds the broadcast open while it is published, so end it here.
    producer.close();
    result?;
    Ok(())
}

/// Holds the broadcast and its sources open until the user interrupts.
fn wait_for_ctrl_c(
    rt: &tokio::runtime::Runtime,
    node: SharedNode,
    broadcast: LocalBroadcast,
    sources: source::Opened,
) -> Result {
    println!("press Ctrl+C to stop");
    rt.block_on(async move {
        tokio::signal::ctrl_c().await?;
        broadcast.close();
        broadcast.closed().await;
        drop(sources);
        node.shutdown().await;
        Ok(())
    })
}

mod preview {
    //! The preview window, with the captured frames and a source picker.
    //!
    //! The picker swaps the device without restarting the broadcast.

    use eframe::egui;
    use iroh_live::media::{LocalBroadcast, VideoSource};
    use moq_ext_egui::{
        VideoView,
        overlay::{DebugOverlay, StatCategory, fit_to_aspect},
    };
    use n0_error::{Result, anyerr};
    use tracing::{info, warn};

    use crate::{
        args::{CaptureArgs, PublishArgs},
        source::{self, Opened},
        source_spec::{TestPattern, VideoSourceSpec},
        transport::SharedNode,
        ui::Pending,
    };

    /// Opens the preview window and runs it until it closes.
    pub(super) fn run(
        node: SharedNode,
        broadcast: LocalBroadcast,
        sources: Opened,
        ticket: String,
        args: &PublishArgs,
    ) -> Result {
        let capture = args.capture.clone();
        crate::ui::run_window("irl publish", args.fullscreen, move |cc| {
            let view = VideoView::new(
                &cc.egui_ctx,
                "preview",
                sources.video.as_ref().map(VideoSource::frames),
                cc.wgpu_render_state.as_ref(),
            );
            PreviewApp {
                node,
                broadcast,
                ticket,
                view,
                picker: SourcePicker::new(capture, sources),
                overlay: DebugOverlay::new(&[StatCategory::Capture, StatCategory::Net]),
            }
        })
    }

    /// The `irl publish --preview` window.
    struct PreviewApp {
        node: SharedNode,
        broadcast: LocalBroadcast,
        ticket: String,
        view: VideoView,
        picker: SourcePicker,
        overlay: DebugOverlay,
    }

    impl eframe::App for PreviewApp {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let ctx = ui.ctx().clone();
            crate::ui::escape_leaves_fullscreen(&ctx);

            if let Some(frames) = self.picker.poll() {
                self.view.set_frames(frames);
            }

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            crate::ui::top_bar(ui, &ctx, &self.ticket);

            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            let size = fit_to_aspect(available, 16.0 / 9.0);
            let image = self.view.render();
            ui.centered_and_justified(|ui| ui.add_sized(size, image));

            self.overlay.show_broadcast(ui, video_rect, &self.broadcast);

            crate::ui::control_panel(&ctx, "publish-controls", |ui| {
                self.picker.ui(ui, &self.broadcast);
            });
        }

        fn on_exit(&mut self) {
            info!("exit");
            crate::ui::shutdown_publish_blocking(&self.node, &self.broadcast);
        }
    }

    /// A source the picker can switch to.
    ///
    /// Only defaults are offered. Use `--video` to pick a specific device.
    /// The other capture and encoding flags apply to every entry.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum PickedSource {
        Camera,
        Screen,
        Test,
        None,
    }

    impl PickedSource {
        /// Every choice, in menu order.
        const ALL: [Self; 4] = [Self::Camera, Self::Screen, Self::Test, Self::None];

        /// Returns the menu label.
        fn label(self) -> &'static str {
            match self {
                Self::Camera => "Camera",
                Self::Screen => "Screen",
                Self::Test => "Test pattern",
                Self::None => "No video",
            }
        }

        /// Returns the `--video` specifier of this entry.
        fn spec(self) -> VideoSourceSpec {
            match self {
                Self::Camera => VideoSourceSpec::Camera(None),
                Self::Screen => VideoSourceSpec::Display(None),
                Self::Test => VideoSourceSpec::Test(TestPattern::default()),
                Self::None => VideoSourceSpec::None,
            }
        }

        /// Returns the entry matching a `--video` specifier, if any.
        ///
        /// Specific devices, windows, and apps have no entry. The picker then
        /// shows the flag text.
        fn from_spec(spec: &VideoSourceSpec) -> Option<Self> {
            match spec {
                VideoSourceSpec::Camera(None) => Some(Self::Camera),
                VideoSourceSpec::Display(None) => Some(Self::Screen),
                VideoSourceSpec::Test(_) => Some(Self::Test),
                VideoSourceSpec::None => Some(Self::None),
                _ => None,
            }
        }
    }

    /// The source combo.
    ///
    /// A switch opens the new source, then sets it on the broadcast in place of
    /// the old one. If the new source fails to open, the old one keeps
    /// publishing.
    #[derive(Debug)]
    struct SourcePicker {
        selected: Option<PickedSource>,
        /// The capture flags.
        ///
        /// `video` is the `--video` text, shown while no entry matches it.
        capture: CaptureArgs,
        error: Option<String>,
        /// The sources publishing now, held to keep them running.
        sources: Opened,
        /// A source switch in flight.
        opening: Option<Pending<Result<Option<VideoSource>>>>,
    }

    impl SourcePicker {
        /// Creates a picker that starts on the source `capture` names.
        fn new(capture: CaptureArgs, sources: Opened) -> Self {
            let selected = PickedSource::from_spec(&capture.video_source());
            Self {
                selected,
                capture,
                error: None,
                sources,
                opening: None,
            }
        }

        /// Draws the source menu and swaps the broadcast video when the choice changes.
        fn ui(&mut self, ui: &mut egui::Ui, broadcast: &LocalBroadcast) {
            ui.label("Video");
            let label = match self.selected {
                Some(source) => source.label(),
                None => &self.capture.video.to_string(),
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
                self.error = None;
                self.apply(ui.ctx(), broadcast);
            }
            if self.opening.is_some() {
                ui.label("opening ...");
            }
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
        }

        /// Collects a finished switch and returns the new frames, if any.
        fn poll(&mut self) -> Option<Option<iroh_live::media::VideoFrames>> {
            let opening = self.opening.as_mut()?;
            let std::task::Poll::Ready(result) = opening.poll() else {
                return None;
            };
            let result = result.unwrap_or_else(|| Err(anyerr!("the source switch was abandoned")));
            self.opening = None;
            match result {
                Ok(video) => {
                    let frames = video.as_ref().map(VideoSource::frames);
                    self.sources.video = video;
                    Some(frames)
                }
                Err(err) => {
                    let err = format!("{err:#}");
                    warn!(error = %err, "source switch failed");
                    self.error = Some(err);
                    None
                }
            }
        }

        /// Opens the selected source in a task and publishes it.
        ///
        /// A new switch aborts one still opening.
        fn apply(&mut self, ctx: &egui::Context, broadcast: &LocalBroadcast) {
            let Some(selected) = self.selected else {
                return;
            };
            let capture = CaptureArgs {
                video: selected.spec(),
                test_source: false,
                ..self.capture.clone()
            };
            let broadcast = broadcast.clone();
            let switch = async move {
                match selected {
                    PickedSource::None => {
                        broadcast.clear_video();
                        Ok(None)
                    }
                    _ => source::configure_video(&broadcast, &capture).await,
                }
            };
            self.opening = Some(Pending::spawn(ctx, switch, drop));
        }
    }
}
