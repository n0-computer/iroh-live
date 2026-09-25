//! A task on a thread of its own, for capture streams that cannot move.
//!
//! On Apple platforms a camera or screen stream holds AVFoundation and
//! ScreenCaptureKit objects, which are not `Send`. The same streams are `Send`
//! on Linux, so the problem only shows in macOS builds.
//!
//! [`spawn`] runs such a future on its own thread in a current-thread runtime.
//! Only the frames it produces leave that thread. The runtime is built on the
//! calling thread, so a runtime or thread that does not start is an error the
//! caller sees.

use std::future::Future;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// A handle that stops its task when dropped.
///
/// A thread cannot be aborted like a tokio task, so dropping this cancels a
/// token the task selects on.
#[derive(Debug)]
pub(crate) struct LocalTask {
    shutdown: CancellationToken,
    #[cfg_attr(
        not(any(feature = "capture", test)),
        expect(dead_code, reason = "only a microphone waits for its thread")
    )]
    joined: Option<oneshot::Receiver<()>>,
}

impl Drop for LocalTask {
    fn drop(&mut self) {
        // Not joined: releasing a device can take a moment, and joining here
        // would block a runtime worker. A caller that needs the device back
        // awaits `joined`.
        self.shutdown.cancel();
    }
}

impl LocalTask {
    /// Waits until the task has finished and released its device.
    ///
    /// Returns at once after that, on every later call. Cancellation safe: a
    /// later call waits again.
    #[cfg_attr(
        not(any(feature = "capture", test)),
        expect(dead_code, reason = "only a microphone waits for its thread")
    )]
    pub(crate) async fn joined(&mut self) {
        let Some(rx) = self.joined.as_mut() else {
            return;
        };
        // By mutable reference, so dropping this future keeps the receiver.
        let _ = rx.await;
        // Cleared only after it resolves, because a resolved
        // `oneshot::Receiver` must not be polled again.
        self.joined = None;
    }
}

/// Runs `make` on a dedicated thread, in a current-thread runtime.
///
/// `make` is called on that thread, so it may build values that are not `Send`.
/// Only the closure has to cross. The returned handle cancels `stop`, which
/// `make` gets to watch.
///
/// Fails if the runtime or the thread cannot be started.
pub(crate) fn spawn<F, Fut>(
    name: &str,
    stop: CancellationToken,
    make: F,
) -> std::io::Result<LocalTask>
where
    F: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let token = stop.clone();
    let (tx, rx) = oneshot::channel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            runtime.block_on(make(token));
            let _ = tx.send(());
        })?;
    Ok(LocalTask {
        shutdown: stop,
        joined: Some(rx),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A cancelled `joined` does not count as finished.
    ///
    /// The next call still waits until the thread releases the device.
    #[tokio::test]
    async fn a_cancelled_join_still_waits_the_next_time() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let mut task = spawn(
            "test-late-release",
            CancellationToken::new(),
            move |_shutdown| async move {
                // Holds the device until the test releases it.
                let _ = released.recv();
            },
        )
        .expect("the thread starts");

        assert!(
            tokio::time::timeout(Duration::from_millis(50), task.joined())
                .await
                .is_err(),
            "the task has not released anything yet",
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), task.joined())
                .await
                .is_err(),
            "a cancelled wait must not report the task as finished",
        );

        release.send(()).expect("the task is still running");
        tokio::time::timeout(Duration::from_secs(5), task.joined())
            .await
            .expect("the task released its device");
        // Every later call returns at once.
        tokio::time::timeout(Duration::from_secs(5), task.joined())
            .await
            .expect("a second call after completion returns");
    }
}
