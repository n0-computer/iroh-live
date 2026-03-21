# Rendering — remaining phases

Phase 1 (GLES2 NV12 shader for Pi) is complete. See
[docs/architecture/rendering.md](../docs/architecture/rendering.md) for
the current three-backend architecture (wgpu, GLES2, DMA-BUF import).

## Phase 2: `FrameRenderer` trait

Unified trait that all backends implement, enabling runtime backend
selection without compile-time feature switches.

- [ ] Define `FrameRenderer` trait with `upload(&mut self, frame)` and
  `draw(&mut self)` split for timing control
- [ ] Implement for `WgpuVideoRenderer`, `GlesRenderer`, `DmaBufImporter`
- [ ] Runtime selection based on GPU capabilities probe

## Phase 3: Android EGL import in Rust

Move the `AHardwareBuffer → EGLImage → GL_TEXTURE_EXTERNAL_OES` import
chain from Kotlin JNI to Rust. Eliminates the JNI boundary for every
frame.

- [ ] Implement in `GlesRenderer` or new `GlesExternalRenderer`
- [ ] Handle EGL context lifecycle from Rust (currently Kotlin-managed)
- [ ] Wire into `moq-media-android` crate

## Phase 4: GLES as a viewer mode

Add `--renderer gles` flag to `watch-wgpu` for headless / low-power
systems. egui examples stay on wgpu (better fit for UI widgets).

- [ ] GLES2 mode in `watch-wgpu` (follow pi-zero windowed pattern)
- [ ] Verify NV12 shader path works on desktop GLES
