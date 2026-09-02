//! Running a publish task whose capture stream cannot cross threads.
//!
//! moq's native capture backends are not all `Send`. On Apple platforms a
//! camera or screen stream holds AVFoundation and ScreenCaptureKit objects, so
//! neither the stream nor a future holding it can be handed to a work-stealing
//! executor. The same code compiles on Linux, where those streams are `Send`,
//! which is why this only surfaces when something builds for macOS.
//!
//! [`spawn`] gives such a future a thread of its own and a current-thread
//! runtime to sit in, which is what moq's own documentation asks for. Nothing
//! that touches the device leaves that thread; only the frames it produces do,
//! and those are `Send`.

use std::future::Future;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// A handle that stops its task when dropped.
///
/// The mirror of `AbortOnDropHandle` for a task that owns a thread: a tokio
/// task can be aborted where it stands, but a thread has to be asked, so this
/// cancels a token the task selects on and lets it unwind.
#[derive(Debug)]
pub struct LocalTask {
    shutdown: CancellationToken,
    joined: Option<oneshot::Receiver<()>>,
}

impl Drop for LocalTask {
    fn drop(&mut self) {
        // Cancelled but deliberately not joined: a capture backend can take a
        // moment to release a device, and blocking a runtime worker on that is
        // worse than letting the thread finish on its own. The token is what
        // guarantees it stops, and [`LocalTask::joined`] is what a caller
        // awaits when it needs the device back before carrying on.
        self.shutdown.cancel();
    }
}

impl LocalTask {
    /// Waits until the task has finished and released its device.
    ///
    /// Returns immediately once it has, and on every later call.
    pub async fn joined(&mut self) {
        if let Some(rx) = self.joined.take() {
            let _ = rx.await;
        }
    }
}

/// Runs `make` on a dedicated thread, in a current-thread runtime.
///
/// `make` is called on that thread, so it may build values that are not `Send`;
/// only the closure itself has to cross, and it is `Send` because it captures
/// only the arguments needed to open the device.
pub fn spawn<F, Fut>(name: &str, make: F) -> LocalTask
where
    F: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    let (tx, rx) = oneshot::channel();

    let spawned = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    tracing::error!(error = %err, "could not start the publish runtime");
                    return;
                }
            };
            runtime.block_on(make(token));
            let _ = tx.send(());
        });

    if let Err(err) = spawned {
        // A thread that will not start is not something a publish can recover
        // from, but it is not worth a panic either: the track simply never
        // produces, which is what the caller sees when a device fails to open.
        tracing::error!(error = %err, "could not start the publish thread");
    }

    LocalTask {
        shutdown,
        joined: Some(rx),
    }
}
