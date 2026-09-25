//! Raspberry Pi Zero 2 demo.
//!
//! Publishes a camera stream over iroh and shows the ticket as a QR code on a
//! Waveshare 2.13" e-paper HAT. It can also watch a remote stream with
//! EGL/GLES2. Builds on Linux only.

#[cfg(not(target_os = "linux"))]
compile_error!("pi-zero-demo only supports Linux");

#[cfg(target_os = "linux")]
mod epaper;
#[cfg(target_os = "linux")]
mod epd_v4;
#[cfg(target_os = "linux")]
mod gles;
#[cfg(target_os = "linux")]
mod publish;
#[cfg(target_os = "linux")]
mod watch;

#[cfg(target_os = "linux")]
mod app {
    use clap::{Parser, Subcommand};
    use iroh::EndpointId;
    use iroh_live::{BroadcastTicket, Live};

    use crate::{epaper, publish, watch};

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
        /// Render a test pattern to HDMI to check the DRM/KMS and GLES2 path.
        FbDemo,
    }

    #[derive(Parser, Debug)]
    struct WatchOpts {
        /// Connection ticket (alternative to --endpoint-id + --name).
        #[clap(conflicts_with = "endpoint_id")]
        ticket: Option<BroadcastTicket>,
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
    }

    pub(crate) async fn run(cli: Cli) -> n0_error::Result {
        match cli.command {
            Command::EpaperDemo => cmd_epaper_demo(),
            Command::Publish(opts) => publish::cmd_publish(opts).await,
            Command::Watch(opts) => cmd_watch(opts).await,
            Command::FbDemo => cmd_fb_demo().await,
        }
    }

    /// Runs a hardware test sequence on the e-paper HAT.
    fn cmd_epaper_demo() -> n0_error::Result {
        println!("step 1/3: checkerboard test pattern");
        epaper::display_test_pattern()?;
        println!("  displayed - you should see a checkerboard now");
        wait_for_enter();

        println!("step 2/3: QR code with dummy data");
        epaper::display_qr("https://iroh.computer/hello-from-pi-zero")?;
        println!("  displayed - you should see a QR code now");
        wait_for_enter();

        println!("step 3/3: clearing display");
        epaper::clear_display()?;
        println!("  done - display should be white");

        Ok(())
    }

    fn wait_for_enter() {
        println!("  press Enter to continue...");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
    }

    /// Renders a test pattern straight to HDMI, without network or camera.
    async fn cmd_fb_demo() -> n0_error::Result {
        use iroh_live_media::VideoSource;
        use moq_video::{Rate, Size};

        let source =
            VideoSource::test_pattern(Size::new(640, 480), Rate::new(30, 1).expect("a valid rate"));
        watch::run_fb_demo(source.frames()).await?;
        Ok(())
    }

    /// Watches a remote broadcast, rendering with EGL/GLES2.
    async fn cmd_watch(opts: WatchOpts) -> n0_error::Result {
        let ticket = match (&opts.ticket, &opts.endpoint_id, &opts.name) {
            (Some(t), None, None) => t.clone(),
            (None, Some(id), Some(name)) => BroadcastTicket::new(*id, name.clone()),
            _ => {
                eprintln!("Usage: watch --ticket <TICKET> or --endpoint-id <ID> --name <NAME>");
                std::process::exit(1);
            }
        };

        println!("connecting to {ticket} ...");
        // The ticket has no addresses. The viewer finds the publisher through
        // the same lookups it announces to: pkarr and DNS, plus mDNS on a
        // network without internet.
        let live = Live::builder(iroh_live::EndpointOptions::from_env()?.bind().await?).spawn();
        let sub = live
            .moq()
            .subscribe(ticket.path(), iroh_live::Reach::Both(ticket.peer()))
            .await?;
        let remote = live.remote_broadcast(&sub);
        let session = sub
            .session()
            .ok_or_else(|| n0_error::anyerr!("the broadcast is not served by a direct session"))?;
        println!("connected!");

        // Wait for a readable catalog, so a bad publisher gives an error and
        // not a black screen. Also watch for close: a closed broadcast sends
        // no catalog update.
        let mut catalog = remote.catalog();
        let described = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::select! {
                catalog = n0_watcher::Watcher::initialized(&mut catalog) => Some(catalog),
                () = remote.closed() => None,
            }
        })
        .await;
        match described {
            Ok(Some(_)) => {}
            Ok(None) => return Err(n0_error::anyerr!("the broadcast closed")),
            Err(_) => {
                return Err(n0_error::anyerr!(
                    "the broadcast sent no catalog this build could read within 15s"
                ));
            }
        }

        // `remote_broadcast` attached the link's signals, so the player picks
        // the rendition on its own.
        let player = remote.play(iroh_live_media::PlayerConfig::default())?;

        if opts.fb {
            watch::run_drm(player, session).await?;
        } else {
            #[cfg(feature = "windowed")]
            watch::run_windowed(player, session, opts.fullscreen)?;
            #[cfg(not(feature = "windowed"))]
            {
                eprintln!(
                    "windowed mode not compiled in - use --fb or build with --features windowed"
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
