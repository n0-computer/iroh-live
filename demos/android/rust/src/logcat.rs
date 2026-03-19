//! Routes [`tracing`] output to Android logcat.
//!
//! Uses `__android_log_write` directly (no external crate) with proper
//! level mapping and null-termination.

use std::io::Write;

use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

unsafe extern "C" {
    fn __android_log_write(prio: i32, tag: *const i8, text: *const i8) -> i32;
}

const TAG: &[u8] = b"iroh_live\0";

const VERBOSE: i32 = 2;
const DEBUG: i32 = 3;
const INFO: i32 = 4;
const WARN: i32 = 5;
const ERROR: i32 = 6;

/// Initializes the global tracing subscriber with logcat output.
///
/// The `filter` string follows [`EnvFilter`] syntax. Returns an error if
/// a subscriber is already set.
pub(crate) fn init(filter: &str) -> Result<(), tracing_subscriber::util::TryInitError> {
    tracing_subscriber::registry()
        .with(EnvFilter::new(filter))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .without_time()
                .with_writer(LogcatMakeWriter),
        )
        .try_init()
}

struct LogcatMakeWriter;

impl<'a> MakeWriter<'a> for LogcatMakeWriter {
    type Writer = LogcatWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogcatWriter::new(INFO)
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        let priority = match *meta.level() {
            Level::ERROR => ERROR,
            Level::WARN => WARN,
            Level::INFO => INFO,
            Level::DEBUG => DEBUG,
            Level::TRACE => VERBOSE,
        };
        LogcatWriter::new(priority)
    }
}

/// Buffers a single log line, writes to logcat on drop.
struct LogcatWriter {
    buf: Vec<u8>,
    priority: i32,
}

impl LogcatWriter {
    fn new(priority: i32) -> Self {
        Self {
            buf: Vec::with_capacity(256),
            priority,
        }
    }
}

impl Write for LogcatWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogcatWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        // Trim trailing newline from fmt layer.
        if self.buf.last() == Some(&b'\n') {
            self.buf.pop();
        }
        // Replace interior nulls to avoid truncation.
        for b in &mut self.buf {
            if *b == 0 {
                *b = b'?';
            }
        }
        self.buf.push(0);
        // SAFETY: TAG is null-terminated, buf was just null-terminated.
        unsafe {
            __android_log_write(self.priority, TAG.as_ptr().cast(), self.buf.as_ptr().cast());
        }
    }
}
