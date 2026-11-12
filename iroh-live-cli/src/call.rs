//! `irl call`: a 1:1 video call.
//!
//! Each side offers its broadcast as `call` to the other peer only, and
//! subscribes to the other's, which `iroh_live::calls` does. A node is called
//! when a peer's `call` path appears in its route table, and a hang-up
//! withdraws the offer. The Android demo does the same, so the two can call
//! each other.
//!
//! Each window shows its own ticket as a QR code and can scan the peer's off
//! the camera. Whoever scans places the call, so no keyboard is needed.

use iroh_live::{
    calls::{CALL, Calls},
    layout::LiveLayout,
    media::LocalBroadcast,
};
use n0_error::Result;
use tracing::info;

use crate::{
    args::CallArgs,
    playback::Speaker,
    source,
    transport::{self, SharedNode},
};

/// Runs the `call` command.
pub(crate) fn run(args: CallArgs, rt: &tokio::runtime::Runtime) -> Result {
    let local = rt.block_on(setup(&args))?;

    // eframe takes over the main thread. The guard lets code on it spawn onto
    // the runtime.
    let _guard = rt.enter();
    window::run(local, args)
}

/// This node's side of the call, and the speaker the peer's side plays on.
struct Local {
    node: SharedNode,
    broadcast: LocalBroadcast,
    /// Who this node's side is offered to.
    calls: Calls,
    sources: source::Opened,
    output: Option<Speaker>,
    ticket: String,
}

/// Binds the endpoint, opens this node's side, and prints the ticket.
async fn setup(args: &CallArgs) -> Result<Local> {
    // Opened first so the microphone can cancel its echo.
    let output = crate::playback::output(None).await?;
    let node = transport::bind_node(true).await?;
    let (node, (broadcast, calls, sources, ticket)) = transport::with_node(node, async |node| {
        // Published once, to nobody: each call offers it to its peer.
        let (calls, broadcast): (Calls, LocalBroadcast) = Calls::new(node, &LiveLayout::default())?;
        let sources = source::configure(&broadcast, &args.capture, output.as_ref()).await?;
        let ticket = LiveLayout::default().ticket(node.id(), CALL)?.to_string();
        println!("your call ticket: {ticket}");
        transport::print_qr(&ticket, args.no_qr);
        info!(ticket, "waiting for a call");
        Ok((broadcast, calls, sources, ticket))
    })
    .await?;
    Ok(Local {
        node,
        broadcast,
        calls,
        sources,
        output,
        ticket,
    })
}

mod camera;
mod screens;
mod window;
