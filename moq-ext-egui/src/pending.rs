//! A task whose one result a window collects on a later frame.

use std::task::Poll;

use n0_future::task::{AbortOnDropHandle, spawn};
use tokio::sync::oneshot;

/// A task whose one result a window collects on a later frame.
///
/// An egui window cannot await. It starts work such as a dial or a device
/// open with [`spawn`](Self::spawn), and asks [`poll`](Self::poll) on each
/// frame until the result is there. The task repaints the window when it
/// finishes, so the window draws the result at once. Dropping the handle
/// aborts the task.
///
/// # Examples
///
/// ```no_run
/// use std::task::Poll;
///
/// use moq_ext_egui::Pending;
///
/// # fn update(ctx: &egui::Context, pending: &mut Option<Pending<u32>>) {
/// let task = pending.get_or_insert_with(|| Pending::spawn(ctx, async { 42 }));
/// if let Poll::Ready(result) = task.poll() {
///     println!("finished with {result:?}");
///     *pending = None;
/// }
/// # }
/// ```
#[derive(Debug)]
pub struct Pending<T> {
    rx: oneshot::Receiver<T>,
    task: AbortOnDropHandle<()>,
}

impl<T: Send + 'static> Pending<T> {
    /// Runs `future` in a task and repaints `ctx` when it finishes.
    ///
    /// A result that arrives after [`cancel`](Self::cancel) or a drop is
    /// dropped.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn spawn(ctx: &egui::Context, future: impl Future<Output = T> + Send + 'static) -> Self {
        Self::spawn_with_late(ctx, future, drop)
    }

    /// Runs `future` in a task, and hands a result nobody waits for any more to `late`.
    ///
    /// For a result that holds something to close, such as a session that
    /// connected after the window gave up on it. The task can only finish
    /// late after [`cancel`](Self::cancel), since a drop aborts it.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    #[must_use]
    pub fn spawn_with_late(
        ctx: &egui::Context,
        future: impl Future<Output = T> + Send + 'static,
        late: impl FnOnce(T) + Send + 'static,
    ) -> Self {
        let (tx, rx) = oneshot::channel();
        let ctx = ctx.clone();
        let task = spawn(async move {
            if let Err(result) = tx.send(future.await) {
                late(result);
            }
            ctx.request_repaint();
        });
        Self {
            rx,
            task: AbortOnDropHandle::new(task),
        }
    }

    /// Returns the result once the task has finished.
    ///
    /// Returns `Ready(None)` for a task that stopped without a result, which
    /// happens after [`cancel`](Self::cancel), and while the runtime shuts
    /// down.
    pub fn poll(&mut self) -> Poll<Option<T>> {
        match self.rx.try_recv() {
            Ok(result) => Poll::Ready(Some(result)),
            Err(oneshot::error::TryRecvError::Empty) => Poll::Pending,
            Err(oneshot::error::TryRecvError::Closed) => Poll::Ready(None),
        }
    }

    /// Stops the task and returns a result it delivered before it stopped.
    pub fn cancel(&mut self) -> Option<T> {
        self.rx.close();
        self.task.abort();
        self.rx.try_recv().ok()
    }

    /// Waits for the result, keeping the task alive until it is there.
    ///
    /// For a task that another task takes over, such as a connection start
    /// that a restart must wait for and close before it starts again, where
    /// [`cancel`](Self::cancel) would drop the result unclosed. Returns `None`
    /// for a task that stopped without a result, as [`poll`](Self::poll) does.
    ///
    /// Cancel safe: dropping the future aborts the task, as dropping the
    /// handle does.
    pub async fn wait(self) -> Option<T> {
        let Self { rx, task } = self;
        let result = rx.await.ok();
        drop(task);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Polls `pending` until it is ready, failing after a second.
    async fn ready<T: Send + 'static>(pending: &mut Pending<T>) -> Option<T> {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Poll::Ready(result) = pending.poll() {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the task finished")
    }

    #[tokio::test]
    async fn a_finished_task_hands_over_its_result() {
        let ctx = egui::Context::default();
        let mut pending = Pending::spawn(&ctx, async { 7 });
        assert_eq!(ready(&mut pending).await, Some(7));
    }

    /// A task awaited elsewhere, here from another task, hands its result over.
    #[tokio::test]
    async fn a_waited_task_hands_over_its_result() {
        let ctx = egui::Context::default();
        let (go_tx, go_rx) = oneshot::channel();
        let pending = Pending::spawn(&ctx, async move { go_rx.await.expect("sent") });
        let waiter = tokio::spawn(pending.wait());
        go_tx.send(9).expect("the task waits");
        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("the task finished")
            .expect("the waiter ran");
        assert_eq!(result, Some(9));
    }

    /// A cancelled task delivers nothing, and reads as ended.
    #[tokio::test]
    async fn a_cancelled_task_reads_as_ended() {
        let ctx = egui::Context::default();
        let mut pending = Pending::spawn(&ctx, std::future::pending::<u32>());
        assert_eq!(pending.poll(), Poll::Pending);
        assert_eq!(pending.cancel(), None);
        assert_eq!(pending.poll(), Poll::Ready(None));
    }
}
