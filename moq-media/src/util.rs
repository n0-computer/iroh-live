use std::{panic::AssertUnwindSafe, thread};

/// Spawns a named OS thread with panic logging.
///
/// If the thread body panics, the panic is caught, logged at `error!`
/// level, and then re-raised so the `JoinHandle` still reports the
/// panic via `join()`. This prevents silent pipeline freezes when a
/// codec or I/O thread hits an unexpected panic.
pub(crate) fn spawn_thread<F, T>(name: impl ToString, f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let name_str = name.to_string();
    let panic_name = name_str.clone();
    let spawn_name = name_str.clone();
    thread::Builder::new()
        .name(name_str)
        .spawn(
            // SAFETY: AssertUnwindSafe is sound here because the closure owns
            // all its captures (moved, not borrowed), and resume_unwind re-raises
            // immediately without touching any state from the unwound closure.
            move || match std::panic::catch_unwind(AssertUnwindSafe(f)) {
                Ok(val) => val,
                Err(payload) => {
                    let msg = payload
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| payload.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("(non-string panic)");
                    tracing::error!(thread = %panic_name, "pipeline thread panicked: {msg}");
                    std::panic::resume_unwind(payload);
                }
            },
        )
        .unwrap_or_else(|e| panic!("failed to spawn thread: {spawn_name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_thread_returns_value() {
        let handle = spawn_thread("test-ok", || 42);
        assert_eq!(handle.join().unwrap(), 42);
    }

    #[test]
    fn spawn_thread_propagates_panic() {
        let handle = spawn_thread("test-panic", || {
            panic!("intentional test panic");
        });
        let result = handle.join();
        assert!(result.is_err(), "panic should propagate through join");
    }
}
