# PipeWire Drop thread join timeout (ON18)

## Problem

`PipeWireScreenCapturer::drop` and `PipeWireCameraCapturer::drop` call
`thread.join()` with no timeout. If the PipeWire main loop is stuck — waiting
on a D-Bus response, a stream buffer that never arrives, or a compositor that
stopped sending frames — the calling thread blocks indefinitely. This is
particularly bad when the capturer is owned by a tokio task or lives on the
main thread, because the entire runtime or UI freezes.

The shutdown mechanism has two stages:

1. `should_stop.store(true, Relaxed)` — sets an `AtomicBool` flag.
2. A dedicated "pw-stopper" thread polls this flag every 50ms and calls
   `pw_main_loop_quit()` to break the main loop.

If stage 2 succeeds, the main loop exits and the capture thread finishes.
The failure modes are:

- The stopper thread is not yet running (spawn failed or not scheduled).
- `pw_main_loop_quit` returns but the main loop doesn't actually exit (e.g.,
  a listener callback re-enters `pw_main_loop_run`).
- The main loop exited but the thread is stuck in cleanup code (dropping
  the `StreamRc`, disconnecting the PipeWire context, closing the fd).

## Proposed fix

Replace the unconditional `join()` with a timed join using `park_timeout`:

```rust
impl Drop for PipeWireScreenCapturer {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            // Park the current thread for up to 2 seconds waiting for
            // the PipeWire thread to finish.
            let parker = std::thread::current();
            let deadline = Instant::now() + Duration::from_secs(2);

            // Spin-check with park_timeout since JoinHandle has no
            // timed join in std.
            while !handle.is_finished() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    tracing::warn!(
                        "PipeWire capture thread did not exit within 2s, detaching"
                    );
                    // Detach: drop the JoinHandle without joining.
                    // The thread will eventually exit when PipeWire
                    // responds to quit, or it will be killed at
                    // process exit.
                    return;
                }
                std::thread::park_timeout(remaining.min(Duration::from_millis(50)));
            }

            // Thread is finished, join to collect the result.
            let _ = handle.join();
        }
    }
}
```

This approach:

- Uses `JoinHandle::is_finished()` (stable since Rust 1.61) to poll.
- Gives the PipeWire thread two seconds to shut down cleanly.
- Detaches the thread on timeout rather than blocking forever. The thread
  will eventually be killed when the process exits. Memory safety is
  maintained because all shared state (`Arc<AtomicBool>`, `SyncSender`) is
  reference-counted and the thread holds no borrows into the capturer.
- The stopper thread also detaches naturally (it only holds an `Arc` clone
  of the stop flag and a raw pointer to the main loop).

Apply the same pattern to both `PipeWireScreenCapturer` and
`PipeWireCameraCapturer`.

## Alternative: signal-based quit

Instead of polling `should_stop` in the stopper thread, use a `pipe()` or
`eventfd()` and add it to the PipeWire main loop as a source. When the fd
becomes readable, the main loop callback calls `pw_main_loop_quit()`. This
removes the 50ms poll latency and is more reliable because the quit signal
is delivered through the main loop's own event mechanism.

This is a better long-term solution but requires more PipeWire API surface
(adding a spa source to the main loop). The timed-join approach above is a
minimal fix that addresses the blocking-forever problem without changing the
PipeWire integration.

## Scope

- `rusty-capture/src/platform/linux/pipewire.rs`: both `Drop` impls
- No new dependencies required
- No API changes

## Risk

Low. The worst case is a leaked thread that runs until process exit. The
current worst case is a permanently blocked thread that prevents shutdown.
