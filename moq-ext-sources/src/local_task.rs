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

use tokio_util::sync::CancellationToken;

/// A handle that stops its task when dropped.
///
/// A thread cannot be aborted like a tokio task, so dropping this cancels a
/// token the task selects on.
#[derive(Debug)]
pub struct LocalTask {
    shutdown: CancellationToken,
    /// Cancelled once the thread has finished.
    ///
    /// This includes a thread that panicked or never started.
    released: CancellationToken,
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
    pub async fn joined(&self) {
        self.released.cancelled().await;
    }

    /// Returns a token that is cancelled once the thread has finished.
    ///
    /// It outlives this handle, so a caller can wait for the release after the
    /// handle is gone.
    #[cfg(feature = "capture")]
    pub(crate) fn released(&self) -> CancellationToken {
        self.released.clone()
    }
}

/// Runs `make` on a dedicated thread, in a current-thread runtime.
///
/// `make` is called on that thread, so it may build values that are not `Send`.
/// Only the closure has to cross. The returned handle cancels `stop`, which
/// `make` gets to watch.
///
/// # Errors
///
/// Fails if the runtime or the thread cannot be started.
pub fn spawn<F, Fut>(name: &str, stop: CancellationToken, make: F) -> std::io::Result<LocalTask>
where
    F: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let token = stop.clone();
    let released = CancellationToken::new();
    // Dropped with the closure, so a panic or a thread that does not start
    // also counts as released.
    let release = released.clone().drop_guard();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            runtime.block_on(make(token));
            // The runtime may still own tasks that hold the device.
            drop(runtime);
            drop(release);
        })?;
    Ok(LocalTask {
        shutdown: stop,
        released,
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
        let (release, released) = std::sync::mpsc::sync_channel::<()>(1);
        let task = spawn(
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

    /// A task that panics still releases, so nobody waits on it forever.
    #[tokio::test]
    async fn a_panicked_task_counts_as_released() {
        let task = spawn("test-panic", CancellationToken::new(), |_shutdown| async {
            panic!("the device driver crashed");
        })
        .expect("the thread starts");
        tokio::time::timeout(Duration::from_secs(5), task.joined())
            .await
            .expect("the panic released the task");
    }
}
