//! Helpers to pass an `Arc<Mutex<T>>` across JNI as an `i64`.
//!
//! JNI bridges often keep Rust state on the Kotlin side as an opaque `jlong`.
//! These helpers keep the pointer casts and reference counting in one place.

use std::{
    mem::ManuallyDrop,
    sync::{Arc, Mutex},
};

/// Converts an `Arc<Mutex<T>>` into an `i64` to store as a JNI `jlong`.
///
/// The `Arc` is leaked. Use [`from_i64`] to borrow it and [`take_i64`] to take
/// it back.
pub fn to_i64<T>(handle: Arc<Mutex<T>>) -> i64 {
    Arc::into_raw(handle) as i64
}

/// Returns a clone of the `Arc` behind a handle, leaving the handle valid.
///
/// # Safety
///
/// `handle` must come from [`to_i64`] and must not have been passed to
/// [`take_i64`] yet.
pub unsafe fn from_i64<T>(handle: i64) -> Arc<Mutex<T>> {
    let arc = ManuallyDrop::new(unsafe { Arc::from_raw(handle as *const Mutex<T>) });
    Arc::clone(&arc)
}

/// Takes back the `Arc` behind a handle.
///
/// # Safety
///
/// `handle` must come from [`to_i64`]. It must not be used after this call.
pub unsafe fn take_i64<T>(handle: i64) -> Arc<Mutex<T>> {
    unsafe { Arc::from_raw(handle as *const Mutex<T>) }
}
