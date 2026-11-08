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
    use iroh_live::{
        BroadcastTicket, Node,
        layout::LiveLayout,
        media::{
            VideoSource, remote_broadcast,
            video::{FrameRate, Size},
        },
    };

    use crate::{epaper, publish, watch};

    /// Command line of the Pi Zero 2 demo.
    #[derive(Parser)]
    #[command(about = "Pi Zero 2 demo: camera streaming and e-paper QR ticket")]
    pub(crate) struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    /// The demo subcommands.
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

    /// Options of the `watch` subcommand.
    #[derive(Parser, Debug)]
    struct WatchOpts {
        /// The broadcast ticket.
        ticket: BroadcastTicket,
        /// Render direct to HDMI framebuffer via DRM/KMS (no window system).
        #[clap(long)]
        fb: bool,
        /// Start in fullscreen mode (windowed mode only, ignored with --fb).
        #[clap(long)]
        fullscreen: bool,
    }

    /// Runs the subcommand that `cli` selects.
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

    /// Blocks until the user presses Enter.
    fn wait_for_enter() {
        println!("  press Enter to continue...");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
    }

    /// Renders a test pattern straight to HDMI, without network or camera.
    async fn cmd_fb_demo() -> n0_error::Result {
        let source = VideoSource::test_pattern(
            Size::new(640, 480),
            FrameRate::new(30, 1).expect("a valid rate"),
        );
        watch::run_fb_demo(source.frames()).await?;
        Ok(())
    }

    /// Watches a remote broadcast, rendering with EGL/GLES2.
    ///
    /// Shuts the node down and closes the endpoint however the playback ends,
    /// so the publisher sees the session close instead of timing out.
    async fn cmd_watch(opts: WatchOpts) -> n0_error::Result {
        // The ticket has no addresses. The viewer finds the publisher through
        // the same lookups it announces to: pkarr and DNS, plus mDNS on a
        // network without internet.
        let endpoint = iroh_live::EndpointOptions::from_env()?.bind().await?;
        // Only dials, so it needs no router, and closes the endpoint itself.
        let node = Node::new(endpoint.clone(), LiveLayout::default().config(&[]));
        let result = watch_with(&node, opts).await;
        node.shutdown().await;
        endpoint.close().await;
        result
    }

    /// Subscribes `node` to the ticket's broadcast and plays it until it ends.
    async fn watch_with(node: &Node, opts: WatchOpts) -> n0_error::Result {
        let ticket = opts.ticket;
        println!("connecting to {ticket} ...");
        let subscription = node.subscribe_ticket(&ticket).await?;
        let remote = remote_broadcast(&subscription);
        println!("connected!");

        // Wait for a readable catalog, so a bad publisher gives an error and
        // not a black screen. Also watch for close: a closed broadcast sends
        // no catalog update.
        let mut catalog = remote.catalog();
        let described = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::select! {
                catalog = catalog.wait_for(Option::is_some) => catalog.ok().map(|_| ()),
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
        let player = remote.play(iroh_live::media::PlayerConfig::default())?;

        if opts.fb {
            watch::run_drm(player).await?;
        } else {
            #[cfg(feature = "windowed")]
            watch::run_windowed(player, opts.fullscreen)?;
            #[cfg(not(feature = "windowed"))]
            return Err(n0_error::anyerr!(
                "this build has no windowed mode: use --fb, or build with --features windowed"
            ));
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
