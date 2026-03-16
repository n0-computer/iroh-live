//! JNI handle helpers for `Arc<Mutex<T>>` ↔ `i64` conversion.
//!
//! Android JNI bridges commonly store Rust session state as an opaque `jlong`
//! (i64) pointer on the Java/Kotlin side. These helpers centralize the
//! pointer arithmetic and `Arc` reference counting to avoid common mistakes.

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

/// Converts an `Arc<Mutex<T>>` into an `i64` suitable for storing as a JNI `jlong`.
///
/// The `Arc` is leaked (its reference count is not decremented). Use
/// [`from_i64`] to borrow the handle and [`take_i64`] to consume it.
pub fn to_i64<T>(handle: Arc<Mutex<T>>) -> i64 {
    Arc::into_raw(handle) as i64
}

/// Recovers a cloned `Arc` from an `i64` without consuming the original.
///
/// # Safety
///
/// The pointer must have been created by [`to_i64`] and must not have been
/// freed yet via [`take_i64`].
pub unsafe fn from_i64<T>(handle: i64) -> Arc<Mutex<T>> {
    let arc = ManuallyDrop::new(unsafe { Arc::from_raw(handle as *const Mutex<T>) });
    Arc::clone(&arc)
}

/// Takes ownership of the handle, decrementing the `Arc` reference count.
///
/// # Safety
///
/// The pointer must have been created by [`to_i64`] and must not be used
/// after this call.
pub unsafe fn take_i64<T>(handle: i64) -> Arc<Mutex<T>> {
    unsafe { Arc::from_raw(handle as *const Mutex<T>) }
}
