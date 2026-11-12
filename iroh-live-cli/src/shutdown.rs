//! Ctrl+C handling and the bounded shutdown every command shares.
//!
//! The first Ctrl+C starts a graceful shutdown: rooms are left, broadcasts
//! closed, and the endpoint closed, so that peers learn at once that this node
//! is gone. A watchdog exits the process if that takes longer than [`GRACE`],
//! and logs the step it was waiting on. A second Ctrl+C exits at once.
//!
//! The watchdog is an OS thread, so it fires even if the runtime is stuck or
//! already shutting down. A command that ends on its own, or a window that is
//! closed, starts the same bounded shutdown through [`Teardown`].
//!
//! Every early exit goes through [`exit_at_once`], which skips the process's
//! exit handlers. Those tear down the GPU driver and the audio libraries, and
//! running them while the window thread still renders aborts the process.

use std::{
    io::{self, Write},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use iroh_live::{media::LocalBroadcast, rooms::Room};
use tracing::{debug, info, warn};

use crate::transport::SharedNode;

/// How long a graceful shutdown may take before the process exits anyway.
pub(crate) const GRACE: Duration = Duration::from_secs(5);

/// The exit code of a process that a second Ctrl+C interrupted, 128 + SIGINT.
const INTERRUPTED: i32 = 130;

/// The exit code of a process whose graceful shutdown ran out of time.
const TIMED_OUT: i32 = 1;

/// Whether a shutdown has begun.
static BEGUN: AtomicBool = AtomicBool::new(false);

/// Whether a window's `on_exit` has run, which means its pass loop is alive.
static WINDOW_CLOSING: AtomicBool = AtomicBool::new(false);

/// The shutdown step that runs now, which the watchdog reports.
static STEP: Mutex<&str> = Mutex::new("starting the shutdown");

/// Waits for Ctrl+C and starts the bounded shutdown.
///
/// Once this returns, a second Ctrl+C exits the process at once. A Ctrl+C
/// that arrives after a shutdown began some other way, for example because a
/// window closed, also exits at once.
///
/// # Errors
///
/// Fails if the signal handler cannot be installed. Ctrl+C then keeps its
/// default behaviour of ending the process.
pub(crate) async fn ctrl_c() -> io::Result<()> {
    tokio::signal::ctrl_c().await?;
    if !begin("interrupted") {
        exit_now();
    }
    println!(
        "shutting down, press Ctrl+C again to exit at once (giving up after {}s)",
        GRACE.as_secs()
    );
    Ok(())
}

/// Starts the bounded shutdown for `reason`.
///
/// Arms the watchdog, and makes the next Ctrl+C exit at once. Returns `false`
/// if a shutdown had already begun, in which case nothing is armed a second
/// time.
pub(crate) fn begin(reason: &'static str) -> bool {
    if BEGUN.swap(true, Ordering::SeqCst) {
        return false;
    }
    info!(reason, grace_secs = GRACE.as_secs(), "shutting down");
    // Installed only now. tokio delivers a signal to the listeners that exist
    // when it arrives, so after a Ctrl+C this one sees the next press only.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async {
                if tokio::signal::ctrl_c().await.is_ok() {
                    exit_now();
                }
            });
        }
        Err(err) => warn!(error = %err, "cannot listen for a second Ctrl+C"),
    }
    let started = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("irl-shutdown".into())
        .spawn(move || {
            std::thread::sleep(GRACE);
            warn!(
                waiting_on = current_step(),
                elapsed = ?started.elapsed(),
                "the graceful shutdown did not finish in time, exiting anyway"
            );
            exit_at_once(TIMED_OUT);
        });
    if let Err(err) = spawned {
        warn!(error = %err, "cannot start the shutdown watchdog, the shutdown is unbounded");
    }
    true
}

/// Records that the shutdown has reached `what`, for the watchdog to report.
pub(crate) fn step(what: &'static str) {
    debug!(step = what, "shutdown step");
    *STEP.lock().expect("poisoned") = what;
}

/// Returns whether a window has started to close, see [`Teardown::run_blocking`].
pub(crate) fn window_closing() -> bool {
    WINDOW_CLOSING.load(Ordering::SeqCst)
}

/// Returns the step the shutdown last reached.
fn current_step() -> &'static str {
    *STEP.lock().expect("poisoned")
}

/// Exits the process at once, for a second Ctrl+C.
fn exit_now() -> ! {
    warn!(
        waiting_on = current_step(),
        "interrupted again, exiting without finishing the shutdown"
    );
    exit_at_once(INTERRUPTED);
}

/// Ends the process with `code` from any thread, without running exit handlers.
///
/// [`std::process::exit`] runs the C exit handlers, among them the GPU
/// driver's and the Vulkan loader's, while other threads keep running. A
/// window thread that renders meanwhile then aborts in the driver. This
/// flushes the standard streams and leaves through `_exit`, so the kernel
/// releases the devices, sockets and files instead.
pub(crate) fn exit_at_once(code: i32) -> ! {
    io::stdout().flush().ok();
    io::stderr().flush().ok();
    #[cfg(unix)]
    {
        // SAFETY: `_exit` takes no pointers and does not return.
        unsafe { libc::_exit(code) }
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

/// What a command closes when it shuts down, in order.
///
/// Cheap to clone, so a window can hand one copy to its Ctrl+C handler and
/// keep another for its `on_exit`.
#[derive(Debug, Clone)]
pub(crate) struct Teardown {
    node: SharedNode,
    room: Option<Room>,
    broadcast: Option<LocalBroadcast>,
}

impl Teardown {
    /// Creates a teardown that shuts `node` down.
    pub(crate) fn new(node: SharedNode) -> Self {
        Self {
            node,
            room: None,
            broadcast: None,
        }
    }

    /// Leaves `room` first, which tells the other members at once.
    pub(crate) fn with_room(mut self, room: Room) -> Self {
        self.room = Some(room);
        self
    }

    /// Closes `broadcast` before the node, so its subscribers see it end.
    pub(crate) fn with_broadcast(mut self, broadcast: LocalBroadcast) -> Self {
        self.broadcast = Some(broadcast);
        self
    }

    /// Leaves the room, closes the broadcast, and shuts the node down.
    ///
    /// Starts the bounded shutdown if nothing has yet, so that a command
    /// which ended on its own cannot stall here either.
    pub(crate) async fn run(&self) {
        begin("the command ended");
        if let Some(room) = &self.room {
            step("leaving the room");
            room.leave().await;
        }
        if let Some(broadcast) = &self.broadcast {
            step("closing the broadcast");
            broadcast.close();
            broadcast.closed().await;
        }
        self.node.shutdown().await;
        step("done");
    }

    /// Runs the teardown from eframe's `on_exit`, outside any async context.
    ///
    /// The teardown is done already when Ctrl+C closed the window, and then
    /// this returns at once.
    pub(crate) fn run_blocking(&self) {
        WINDOW_CLOSING.store(true, Ordering::SeqCst);
        begin("the window was closed");
        tokio::runtime::Handle::current().block_on(self.run());
        // eframe drops the app and the renderer next.
        step("closing the window");
    }
}
