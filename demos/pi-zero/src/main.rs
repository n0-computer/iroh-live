/// Raspberry Pi Zero 2 demo: publish a camera stream over iroh and display
/// the connection ticket as a QR code on a Waveshare 2.13" e-paper HAT.
/// Also supports watching a remote stream with EGL/GLES2 rendering.
///
/// This binary only builds and runs on Linux (ARM64 target).
#[cfg(not(target_os = "linux"))]
compile_error!("pi-zero-demo only supports Linux");

#[cfg(target_os = "linux")]
mod codec_test;
#[cfg(target_os = "linux")]
mod epaper;
#[cfg(target_os = "linux")]
mod epd_v4;
#[cfg(target_os = "linux")]
mod publish;
#[cfg(target_os = "linux")]
mod watch;

#[cfg(target_os = "linux")]
mod app {
    use clap::{Parser, Subcommand};
    use iroh::{Endpoint, EndpointId};
    use iroh_live::{
        Live,
        media::format::{DecoderBackend, PlaybackConfig},
        ticket::LiveTicket,
    };

    use crate::{codec_test, epaper, publish, watch};

    #[derive(Parser)]
    #[command(about = "Pi Zero 2 demo: camera streaming + e-paper QR ticket")]
    pub(crate) struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Subcommand)]
    enum Command {
        /// Test the e-paper display with a "hello" label and a bogus QR code.
        EpaperDemo,
        /// Publish the camera stream and show the ticket QR on e-paper.
        Publish(publish::PublishOpts),
        /// Watch a remote stream, rendering with EGL/GLES2.
        Watch(WatchOpts),
        /// Render a test pattern directly to HDMI (no network, no window system).
        FbDemo,
        /// Run V4L2 codec tests (encoder, decoder, roundtrip) on-device.
        CodecTest(codec_test::CodecTestOpts),
    }

    #[derive(Parser, Debug)]
    struct WatchOpts {
        /// Connection ticket (alternative to --endpoint-id + --name).
        #[clap(conflicts_with = "endpoint_id")]
        ticket: Option<LiveTicket>,
        /// Remote endpoint ID (requires --name).
        #[clap(long, conflicts_with = "ticket", requires = "name")]
        endpoint_id: Option<EndpointId>,
        /// Broadcast name.
        #[clap(long, conflicts_with = "ticket", requires = "endpoint_id")]
        name: Option<String>,
        /// Render direct to HDMI framebuffer via DRM/KMS (no window system).
        #[clap(long)]
        fb: bool,
        /// Start in fullscreen mode (windowed mode only, ignored with --fb).
        #[clap(long)]
        fullscreen: bool,
        /// Decoder: "auto" (try HW then SW), "software", or "ffmpeg".
        #[clap(long, default_value = "auto")]
        decoder: String,
    }

    pub(crate) async fn run(cli: Cli) -> n0_error::Result {
        match cli.command {
            Command::EpaperDemo => cmd_epaper_demo(),
            Command::Publish(opts) => publish::cmd_publish(opts).await,
            Command::Watch(opts) => cmd_watch(opts).await,
            Command::FbDemo => cmd_fb_demo(),
            Command::CodecTest(opts) => Ok(codec_test::run(opts)?),
        }
    }

    /// Runs a hardware test sequence on the e-paper HAT.
    fn cmd_epaper_demo() -> n0_error::Result {
        println!("step 1/3: checkerboard test pattern");
        epaper::display_test_pattern()?;
        println!("  displayed — you should see a checkerboard now");
        wait_for_enter();

        println!("step 2/3: QR code with dummy data");
        epaper::display_qr("https://iroh.computer/hello-from-pi-zero")?;
        println!("  displayed — you should see a QR code now");
        wait_for_enter();

        println!("step 3/3: clearing display");
        epaper::clear_display()?;
        println!("  done — display should be white");

        Ok(())
    }

    fn wait_for_enter() {
        println!("  press Enter to continue...");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
    }

    /// Renders a test pattern directly to HDMI — no network, no window system.
    fn cmd_fb_demo() -> n0_error::Result {
        use moq_media::{subscribe::VideoTrack, test_util::TestVideoSource};
        use tokio_util::sync::CancellationToken;

        let source = TestVideoSource::new(640, 480).with_fps(30.0);
        let shutdown = CancellationToken::new();
        let video_track = VideoTrack::from_video_source("test".into(), shutdown, source);

        watch::run_fb_demo(video_track)?;
        Ok(())
    }

    /// Watches a remote broadcast, rendering with EGL/GLES2.
    async fn cmd_watch(opts: WatchOpts) -> n0_error::Result {
        let ticket = match (&opts.ticket, &opts.endpoint_id, &opts.name) {
            (Some(t), None, None) => t.clone(),
            (None, Some(id), Some(name)) => LiveTicket::new(*id, name.clone()),
            _ => {
                eprintln!("Usage: watch --ticket <TICKET> or --endpoint-id <ID> --name <NAME>");
                std::process::exit(1);
            }
        };
        let backend = match opts.decoder.as_str() {
            "auto" | "hardware" | "hw" => DecoderBackend::Auto,
            "software" | "sw" => DecoderBackend::Software,
            "ffmpeg" => {
                eprintln!("ffmpeg decoder has been removed from rusty-codecs");
                std::process::exit(1);
            }
            other => {
                eprintln!("Unknown decoder: {other}. Use 'auto', 'software', or 'ffmpeg'");
                std::process::exit(1);
            }
        };

        let audio_ctx = moq_media::test_util::NullAudioBackend;

        println!("connecting to {ticket} ...");
        let endpoint = Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let live = Live::new(endpoint);
        let playback_config = PlaybackConfig {
            backend,
            ..Default::default()
        };
        let (session, track) = live
            .subscribe_media(
                ticket.endpoint,
                &ticket.broadcast_name,
                &audio_ctx,
                playback_config,
            )
            .await?;
        println!("connected!");

        let video_track = track.video.expect("no video track in broadcast");

        if opts.fb {
            watch::run_drm(video_track, session).await?;
        } else {
            #[cfg(feature = "windowed")]
            watch::run_windowed(video_track, session, opts.fullscreen)?;
            #[cfg(not(feature = "windowed"))]
            {
                eprintln!(
                    "windowed mode not compiled in — use --fb or build with --features windowed"
                );
                std::process::exit(1);
            }
        }

        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> n0_error::Result {
    tracing_subscriber::fmt::init();
    let cli = <app::Cli as clap::Parser>::parse();
    app::run(cli).await
}
