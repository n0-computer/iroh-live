//! Minimal safe wrapper over rav1d's C API.
//!
//! rav1d is a pure Rust port of dav1d (AV1 decoder) that only exposes
//! `unsafe extern "C"` functions. This module provides a safe interface
//! for the subset we use.

use std::error;
use std::ffi::c_int;
use std::fmt;
use std::mem::MaybeUninit;
use std::ptr;
use std::ptr::NonNull;
use std::slice;

use rav1d::Dav1dResult;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::picture::Dav1dPicture;

/// YUV plane component.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlanarImageComponent {
    Y = 0,
    U = 1,
    V = 2,
}

/// Decoder error.
#[derive(Debug)]
pub(crate) struct Error(c_int);

impl Error {
    /// Returns `true` if the decoder needs more data (EAGAIN).
    pub(crate) fn is_again(&self) -> bool {
        self.0 == -(libc::EAGAIN as c_int)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rav1d error code {}", self.0)
    }
}

impl error::Error for Error {}

fn check(r: Dav1dResult) -> Result<(), Error> {
    if r.0 == 0 { Ok(()) } else { Err(Error(r.0)) }
}

/// Decoder settings.
pub(crate) struct Settings {
    inner: Dav1dSettings,
}

impl Settings {
    pub(crate) fn new() -> Self {
        let mut inner = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: dav1d_default_settings writes valid defaults into the pointer.
        unsafe { rav1d::dav1d_default_settings(NonNull::new(inner.as_mut_ptr()).unwrap()) };
        Self {
            inner: unsafe { inner.assume_init() },
        }
    }

    pub(crate) fn set_n_threads(&mut self, n: u32) {
        self.inner.n_threads = n as c_int;
    }

    pub(crate) fn set_max_frame_delay(&mut self, delay: u32) {
        self.inner.max_frame_delay = delay as c_int;
    }
}

/// AV1 decoder.
pub(crate) struct Decoder {
    ctx: Option<Dav1dContext>,
}

// SAFETY: Dav1dContext is RawArc<Rav1dContext> with internal locking.
unsafe impl Send for Decoder {}

impl Decoder {
    pub(crate) fn with_settings(settings: &Settings) -> Result<Self, Error> {
        let mut ctx: Option<Dav1dContext> = None;
        // SAFETY: ctx and settings are valid pointers.
        let result = unsafe {
            rav1d::dav1d_open(
                NonNull::new(&mut ctx as *mut _),
                NonNull::new(&settings.inner as *const Dav1dSettings as *mut Dav1dSettings),
            )
        };
        check(result)?;
        Ok(Self { ctx })
    }

    pub(crate) fn send_data(&mut self, data: &[u8]) -> Result<(), Error> {
        let mut dav1d_data = Dav1dData::default();
        // SAFETY: dav1d_data_create allocates an internal buffer of `data.len()` bytes
        // and returns a pointer to it. The Dav1dData takes ownership of the allocation.
        let ptr = unsafe {
            rav1d::dav1d_data_create(NonNull::new(&mut dav1d_data as *mut _), data.len())
        };
        if ptr.is_null() {
            return Err(Error(-(libc::ENOMEM as c_int)));
        }
        // SAFETY: ptr points to data.len() bytes allocated by dav1d_data_create.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };

        // SAFETY: ctx is valid, dav1d_data contains the allocated buffer.
        let result =
            unsafe { rav1d::dav1d_send_data(self.ctx, NonNull::new(&mut dav1d_data as *mut _)) };
        // Clean up any unconsumed data.
        if dav1d_data.sz > 0 {
            // SAFETY: dav1d_data still holds a valid ref.
            unsafe { rav1d::dav1d_data_unref(NonNull::new(&mut dav1d_data as *mut _)) };
        }
        check(result)
    }

    pub(crate) fn get_picture(&mut self) -> Result<Picture, Error> {
        let mut pic = Dav1dPicture::default();
        // SAFETY: ctx is valid, pic is default-initialized.
        let result =
            unsafe { rav1d::dav1d_get_picture(self.ctx, NonNull::new(&mut pic as *mut _)) };
        check(result)?;
        Ok(Picture { inner: pic })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if self.ctx.is_some() {
            // SAFETY: ctx was opened with dav1d_open and has not been closed.
            unsafe { rav1d::dav1d_close(NonNull::new(&mut self.ctx as *mut _)) };
        }
    }
}

/// A decoded picture. Calls `dav1d_picture_unref` on drop.
pub(crate) struct Picture {
    inner: Dav1dPicture,
}

// SAFETY: Dav1dPicture data is Arc-based internally.
unsafe impl Send for Picture {}

impl Picture {
    pub(crate) fn width(&self) -> u32 {
        self.inner.p.w as u32
    }

    pub(crate) fn height(&self) -> u32 {
        self.inner.p.h as u32
    }

    /// Returns the raw plane data as a byte slice.
    pub(crate) fn plane(&self, component: PlanarImageComponent) -> &[u8] {
        let idx = component as usize;
        let Some(data_ptr) = self.inner.data[idx] else {
            return &[];
        };
        let stride = self.stride(component) as usize;
        if stride == 0 {
            return &[];
        }
        let height = match component {
            PlanarImageComponent::Y => self.height() as usize,
            // I420: chroma height = ceil(luma_height / 2)
            _ => (self.height() as usize).div_ceil(2),
        };
        // SAFETY: data_ptr is valid for stride * height bytes while Picture is alive.
        // The Dav1dPicture holds Arc refs to the backing data.
        unsafe { slice::from_raw_parts(data_ptr.as_ptr() as *const u8, stride * height) }
    }

    /// Stride in bytes for the given component.
    pub(crate) fn stride(&self, component: PlanarImageComponent) -> u32 {
        // stride[0] = Y, stride[1] = U and V (shared)
        let s = match component {
            PlanarImageComponent::Y => 0,
            _ => 1,
        };
        self.inner.stride[s] as u32
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: inner was populated by dav1d_get_picture.
        unsafe { rav1d::dav1d_picture_unref(NonNull::new(&mut self.inner as *mut _)) };
    }
}
