//! `irl publish`: publish a capture device or a media file over iroh.
//!
//! Capture sources go through `iroh-live-media`'s encode path, which fans one device
//! out to the simulcast ladder `--renditions` describes. A `file:` source takes
//! the import path instead: its tracks are republished as they already are.

use iroh_live::{Live, media::LocalBroadcast, moq::net::broadcast};
use n0_error::{Result, anyerr};
use tracing::{info, warn};

use crate::{
    args::PublishArgs,
    import::{FileImport, FileSource},
    source,
    source_spec::VideoSourceSpec,
    transport::{self, setup_live},
};

/// Runs the `publish` command.
pub fn run(args: PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    match args.capture.video_source()? {
        VideoSourceSpec::File { path, looping } => {
            publish_file(FileSource::new(path, looping, &args)?, &args, rt)
        }
        // Same reason as a file source: nothing on this path ever holds a
        // picture. `rpicam:raw` is the other half of the choice and does, so
        // it falls through to the ordinary capture path with its preview.
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
) -> Result<(Live, LocalBroadcast, source::Opened, String)> {
    let live = setup_live(!args.transport.no_serve).await?;
    let (live, (broadcast, sources, ticket)) = transport::with_live(live, async |live| {
        let broadcast = LocalBroadcast::new();
        let sources = source::configure(&broadcast, &args.capture, None).await?;
        live.publish(&args.transport.name, &broadcast)?;
        let ticket = transport::advertise(live, &args.transport)?;
        // `--test-source` overrides both flags, so logging what was typed would
        // name a camera that was never opened.
        let (video, audio) = match args.capture.test_source {
            true => ("test", "test"),
            false => (args.capture.video.as_str(), args.capture.audio.as_str()),
        };
        info!(
            name = %args.transport.name,
            video,
            audio,
            "publishing"
        );
        Ok((broadcast, sources, ticket))
    })
    .await?;
    Ok((live, broadcast, sources, ticket))
}

/// Publishes capture devices, optionally alongside a preview window.
fn publish_capture(args: &PublishArgs, rt: &tokio::runtime::Runtime) -> Result {
    // Checked before anything is opened: a build that cannot draw should say so
    // rather than open the camera first and fail afterwards.
    #[cfg(not(feature = "render"))]
    if args.preview {
        return Err(anyerr!(
            "--preview needs the 'render' feature, which this build was \
             compiled without; publish without it, or install a build that has it"
        ));
    }

    let (live, broadcast, sources, ticket) = rt.block_on(setup_capture(args))?;

    if !args.preview {
        return wait_for_ctrl_c(rt, live, broadcast, sources);
    }

    #[cfg(feature = "render")]
    {
        // eframe owns the main thread, so the runtime stays alive only for as
        // long as this guard does.
        let _guard = rt.enter();
        preview::run(live, broadcast, sources, ticket, args)
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = ticket;
        unreachable!("--preview is rejected above in a build without the render feature")
    }
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
    let live = setup_live(!args.transport.no_serve).await?;
    let result = publish_import(&live, source, args).await;
    live.shutdown().await;
    result
}

/// Publishes the file onto `live`, which the caller closes either way.
async fn publish_import(live: &Live, source: FileSource, args: &PublishArgs) -> Result {
    let producer = broadcast::Info::new().produce();
    let import = FileImport::open(producer.clone(), source).await?;
    // Published once the import has created the tracks it republishes, so a
    // subscriber never sees the broadcast without them.
    live.publish(&args.transport.name, producer.consume())?;
    transport::advertise(live, &args.transport)?;
    info!(name = %args.transport.name, "publishing a file");

    println!("press Ctrl+C to stop");
    tokio::select! {
        result = import.run() => result?,
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

/// Holds the broadcast and its sources open until the user interrupts.
fn wait_for_ctrl_c(
    rt: &tokio::runtime::Runtime,
    live: Live,
    broadcast: LocalBroadcast,
    sources: source::Opened,
) -> Result {
    println!("press Ctrl+C to stop");
    rt.block_on(async move {
        tokio::signal::ctrl_c().await?;
        broadcast.close();
        broadcast.closed().await;
        drop(sources);
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
        media::{LocalBroadcast, VideoEncoding, VideoRendition, VideoSource, video},
    };
    use iroh_live_egui::overlay::{DebugOverlay, StatCategory, fit_to_aspect};
    use n0_error::{Result, anyerr};
    use n0_future::task::AbortOnDropHandle;
    use n0_watcher::Watcher as _;
    use tokio::sync::oneshot;
    use tracing::{info, warn};

    use crate::{
        args::PublishArgs, source::Opened, source_spec::VideoSourceSpec, ui::LocalPreview,
    };

    /// Opens the preview window and runs it until it closes.
    ///
    /// The picker starts on the `--video` specifier the broadcast was started
    /// with, so it shows what is already publishing instead of guessing.
    pub(super) fn run(
        live: Live,
        broadcast: LocalBroadcast,
        sources: Opened,
        ticket: String,
        args: &PublishArgs,
    ) -> Result {
        let flag = args.capture.video.clone();
        eframe::run_native(
            "irl publish",
            crate::ui::native_options(args.fullscreen),
            Box::new(move |cc| {
                crate::ui::spawn_ctrl_c_handler(&cc.egui_ctx);
                let view = LocalPreview::new(
                    &cc.egui_ctx,
                    "preview",
                    sources.video.as_ref().map(VideoSource::frames),
                    cc.wgpu_render_state.as_ref(),
                );
                Ok(Box::new(PreviewApp {
                    live,
                    broadcast,
                    ticket,
                    view,
                    picker: SourcePicker::new(&flag, sources),
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
            crate::ui::escape_leaves_fullscreen(&ctx);
            ctx.request_repaint_after(Duration::from_millis(16));

            if let Some(frames) = self.picker.poll() {
                self.view.set_frames(frames);
            }
            self.view.update(&ctx);

            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            crate::ui::top_bar(ui, &ctx, &self.ticket);

            let available = ui.available_size();
            let video_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            let size = fit_to_aspect(available, 16.0 / 9.0);
            let image = self.view.image();
            ui.centered_and_justified(|ui| ui.add_sized(size, image));

            let stats = self.broadcast.stats();
            let status = self.broadcast.status().get();
            self.overlay.show_publish(ui, video_rect, &stats, &status);

            crate::ui::control_panel(&ctx, "publish-controls", |ui| {
                self.picker.ui(ui, &self.broadcast);
            });
        }

        fn on_exit(&mut self) {
            info!("exit");
            crate::ui::shutdown_publish_blocking(&self.live, &self.broadcast);
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
                VideoSourceSpec::Test(_) => Some(Self::Test),
                VideoSourceSpec::None => Some(Self::None),
                _ => None,
            }
        }
    }

    /// The source combo.
    ///
    /// Switching opens the new source first and then sets it, which replaces
    /// whatever was publishing: there is no separate teardown step, and the
    /// catalog keeps the same rendition name across the swap. A source that
    /// will not open leaves the old one publishing.
    #[derive(Debug)]
    struct SourcePicker {
        selected: Option<PickedSource>,
        /// What `--video` said, shown while nothing in the combo matches it.
        flag: String,
        error: Option<String>,
        /// The sources publishing now, held so they keep running.
        sources: Opened,
        /// A source being opened, and where it reports.
        opening: Option<Opening>,
    }

    /// A source switch in flight.
    #[derive(Debug)]
    struct Opening {
        done: oneshot::Receiver<Result<Option<VideoSource>>>,
        _task: AbortOnDropHandle<()>,
    }

    impl SourcePicker {
        fn new(flag: &str, sources: Opened) -> Self {
            let selected = VideoSourceSpec::parse(flag)
                .ok()
                .as_ref()
                .and_then(PickedSource::from_spec);
            Self {
                selected,
                flag: flag.to_string(),
                error: None,
                sources,
                opening: None,
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
                self.error = None;
                self.apply(broadcast);
            }
            if self.opening.is_some() {
                ui.label("opening ...");
            }
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
            }
        }

        /// Collects a finished switch, returning the frames the preview should
        /// draw from now on when the source changed.
        fn poll(&mut self) -> Option<Option<iroh_live::media::VideoFrames>> {
            let opening = self.opening.as_mut()?;
            let result = match opening.done.try_recv() {
                Ok(result) => result,
                Err(oneshot::error::TryRecvError::Empty) => return None,
                Err(oneshot::error::TryRecvError::Closed) => {
                    Err(anyerr!("the source switch was abandoned"))
                }
            };
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

        /// Opens the selected source and publishes it at its own resolution.
        ///
        /// The open runs as a task, since a camera takes a moment; a switch
        /// chosen while another is opening replaces it.
        fn apply(&mut self, broadcast: &LocalBroadcast) {
            let selected = self.selected;
            let broadcast = broadcast.clone();
            let (done, report) = oneshot::channel();
            let task = tokio::spawn(async move {
                let result = open_and_set(&broadcast, selected).await;
                let _ = done.send(result);
            });
            self.opening = Some(Opening {
                done: report,
                _task: AbortOnDropHandle::new(task),
            });
        }
    }

    /// Opens `selected` and sets it on `broadcast`, or clears the video for
    /// no source.
    async fn open_and_set(
        broadcast: &LocalBroadcast,
        selected: Option<PickedSource>,
    ) -> Result<Option<VideoSource>> {
        let source = match selected {
            None | Some(PickedSource::None) => {
                broadcast.clear_video();
                return Ok(None);
            }
            Some(PickedSource::Camera) => {
                VideoSource::capture(video::capture::Config::default()).await?
            }
            Some(PickedSource::Screen) => {
                let mut config = video::capture::Config::default();
                config.source = video::capture::Source::Display(None);
                VideoSource::capture(config).await?
            }
            Some(PickedSource::Test) => crate::source::default_test_pattern(),
        };
        broadcast.set_video(
            source.clone(),
            VideoEncoding::single(VideoRendition::new("video")),
        )?;
        Ok(Some(source))
    }
}
