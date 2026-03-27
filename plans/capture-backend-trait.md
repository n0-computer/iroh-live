# Capture backend trait refactor

## Problem

`rusty-capture/src/lib.rs` repeats the same `#[cfg(...)]` flags for each
backend in four to five places: `list_cameras`, `list_all_cameras`,
`create_camera_backend`, `list_backends` (and the screen equivalents).
Adding a new backend means touching all of them. The PipeWire runtime
check (`pipewire_available()`) adds a further wrinkle because it's not
purely compile-time.

## Proposed design

A trait on zero-sized types, one per backend:

```rust
trait BackendExt: Send + Sync {
    fn backend(&self) -> CaptureBackend;

    /// Runtime availability check. PipeWire returns false when the
    /// daemon isn't running; all others return true.
    fn available(&self) -> bool { true }

    fn cameras(&self) -> Result<Vec<CameraInfo>> { Ok(vec![]) }
    fn monitors(&self) -> Result<Vec<MonitorInfo>> { Ok(vec![]) }

    fn create_camera(
        &self, info: &CameraInfo, config: &CameraConfig,
    ) -> Result<Box<dyn VideoSource>> {
        let _ = (info, config);
        bail!("{} does not support camera capture", self.backend())
    }

    fn create_screen(
        &self, monitor: Option<&MonitorInfo>, config: &ScreenConfig,
    ) -> Result<Box<dyn VideoSource>> {
        let _ = (monitor, config);
        bail!("{} does not support screen capture", self.backend())
    }
}
```

Each backend is a cfg-gated ZST + impl:

```rust
#[cfg(all(target_os = "linux", feature = "v4l2"))]
struct V4l2Back;

#[cfg(all(target_os = "linux", feature = "v4l2"))]
impl BackendExt for V4l2Back {
    fn backend(&self) -> CaptureBackend { CaptureBackend::V4l2 }
    fn cameras(&self) -> Result<Vec<CameraInfo>> { platform::linux::v4l2::cameras() }
    fn create_camera(&self, info: &CameraInfo, config: &CameraConfig)
        -> Result<Box<dyn VideoSource>>
    {
        Ok(Box::new(V4l2CameraCapturer::new(info, config)?))
    }
}
```

Registration collects all backends into a single ordered list. The cfg
flags appear on the struct definition and in this one function:

```rust
fn all_backends() -> Vec<&'static dyn BackendExt> {
    let mut v: Vec<&'static dyn BackendExt> = vec![];
    #[cfg(all(target_os = "linux", feature = "pipewire"))]
    v.push(&PipeWireBack);
    #[cfg(all(target_os = "linux", feature = "v4l2"))]
    v.push(&V4l2Back);
    // ...
    v
}
```

The ordering in `all_backends()` defines the priority cascade. Then the
public dispatch functions collapse to one-liners:

```rust
fn list_cameras() -> Result<Vec<CameraInfo>> {
    // First available backend with cameras (priority cascade).
    for b in all_backends() {
        if b.available() {
            let cams = b.cameras()?;
            if !cams.is_empty() { return Ok(cams); }
        }
    }
    bail!("no camera backend available")
}

fn list_all_cameras() -> Result<Vec<CameraInfo>> {
    let mut all = Vec::new();
    for b in all_backends() {
        if b.available() {
            if let Ok(cams) = b.cameras() { all.extend(cams); }
        }
    }
    Ok(all)
}
```

## Tradeoffs

**Pros:**
- Cfg flags in two places (struct def + registration) instead of four or
  five.
- Adding a backend is one file: ZST, impl, one line in `all_backends()`.
- Dispatch functions become trivial loops, hard to get wrong.

**Cons:**
- ~80 lines of trait boilerplate upfront.
- Dynamic dispatch through `&dyn BackendExt` instead of direct function
  calls. Irrelevant for performance (called once at startup) but adds
  indirection when reading the code.
- PipeWire's `available()` runtime check fits the trait cleanly, but
  PipeWire's placeholder entries (no real enumeration, portal-based) are
  a conceptual mismatch with `cameras()` returning real device lists.

## When to do this

Not urgent with three to four backends. Worth doing when:
- A fifth backend is added (Windows, Android), making the repetition
  painful enough to justify the refactor.
- The per-backend logic diverges further (e.g., format negotiation
  differences) and a trait boundary helps contain it.

Current estimate: small refactor, half a day including tests.
