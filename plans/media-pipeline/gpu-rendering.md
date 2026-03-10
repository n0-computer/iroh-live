# Hardware Video Decoding + Zero-Copy wgpu Rendering

## Context

All video decoding currently produces CPU RGBA buffers (`image::Frame` → `RgbaImage`). The watch example uploads these to the GPU every frame via egui. This means: HW decoder → CPU readback → YUV→RGBA on CPU → GPU upload — wasting bandwidth and latency.

Goal: HW decoded frames stay on GPU, rendered zero-copy via wgpu. SW decoded frames upload as before. Both paths coexist.

---

## Phase 1: Frame Abstraction

Pure refactor, no new dependencies. All existing tests continue to pass.

### New types in `moq-media/src/format.rs`

```rust
/// A decoded video frame, either CPU or GPU resident.
#[derive(derive_more::Debug)]
pub struct DecodedVideoFrame {
    pub buffer: FrameBuffer,
    pub timestamp: Duration,
    /// Lazy CPU download cache for backward-compat `img()`.
    #[debug(skip)]
    cached_rgba: std::cell::OnceCell<RgbaImage>,
}

/// Backing storage for a decoded frame.
#[derive(derive_more::Debug)]
pub enum FrameBuffer {
    Cpu(CpuFrame),
    Gpu(GpuFrame),
}

/// CPU-resident RGBA pixel data.
#[derive(derive_more::Debug, Clone)]
pub struct CpuFrame {
    #[debug(skip)]
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
}

/// GPU-resident frame from a hardware decoder.
#[derive(derive_more::Debug, Clone)]
pub struct GpuFrame {
    #[debug(skip)]
    inner: Arc<dyn GpuFrameInner>,
}

pub trait GpuFrameInner: Send + Sync + std::fmt::Debug + 'static {
    /// Download GPU frame to CPU RGBA buffer (fallback path).
    fn download(&self) -> anyhow::Result<CpuFrame>;
    /// Native GPU pixel format (NV12, I420, etc.).
    fn gpu_pixel_format(&self) -> GpuPixelFormat;
    /// Frame dimensions.
    fn dimensions(&self) -> (u32, u32);
}

#[derive(Debug, Clone, Copy)]
pub enum GpuPixelFormat { Nv12, I420 }
```

### DecodedVideoFrame API

```rust
impl DecodedVideoFrame {
    /// Dimensions — delegates to CpuFrame or GpuFrame.
    pub fn dimensions(&self) -> (u32, u32) {
        match &self.buffer {
            FrameBuffer::Cpu(f) => (f.width, f.height),
            FrameBuffer::Gpu(f) => f.dimensions(),
        }
    }

    /// Backward-compat: lazily downloads GPU frames on first call.
    /// Uses OnceCell (not OnceLock — DecodedVideoFrame is !Sync, same as before).
    pub fn img(&self) -> &RgbaImage {
        self.cached_rgba.get_or_init(|| {
            match &self.buffer {
                FrameBuffer::Cpu(cpu) =>
                    RgbaImage::from_raw(cpu.width, cpu.height, cpu.data.clone()).unwrap(),
                FrameBuffer::Gpu(gpu) => {
                    let cpu = gpu.download().expect("GPU frame download failed");
                    RgbaImage::from_raw(cpu.width, cpu.height, cpu.data).unwrap()
                }
            }
        })
    }

    pub fn is_gpu(&self) -> bool { matches!(&self.buffer, FrameBuffer::Gpu(_)) }
    pub fn gpu_frame(&self) -> Option<&GpuFrame> { ... }
}

impl GpuFrame {
    pub fn download(&self) -> anyhow::Result<CpuFrame> { self.inner.download() }
    pub fn dimensions(&self) -> (u32, u32) { self.inner.dimensions() }
    pub fn gpu_pixel_format(&self) -> GpuPixelFormat { self.inner.gpu_pixel_format() }
}
```

### Update existing decoders

**H264VideoDecoder** (`moq-media/src/codec/h264/decoder.rs`):
- `pop_frame()` currently builds `Frame::from_parts(final_img, 0, 0, frame_delay)` at line 130-131
- Change to build `FrameBuffer::Cpu(CpuFrame { data: final_img.into_raw(), width: sw, height: sh, pixel_format: PixelFormat::Rgba })`
- Remove `image::Frame` usage, use `DecodedVideoFrame { buffer, timestamp, cached_rgba: OnceCell::new() }`

**Av1VideoDecoder** (`moq-media/src/codec/av1/decoder.rs`):
- Same pattern — already produces `RgbaImage`, convert to `CpuFrame`

### Update `run_loop()` (`moq-media/src/subscribe.rs:688-770`)

BGRA swap: only for `FrameBuffer::Cpu` frames:
```rust
if let FrameBuffer::Cpu(ref mut cpu) = frame.buffer {
    if target_pixel_format == PixelFormat::Bgra {
        for pixel in cpu.data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
}
// GPU frames pass through untouched — zero copy
```

Viewport scaling: GPU decoders output at full coded resolution; scaling happens in shader. Existing SW decoders keep their CPU scaling. No change to `run_loop` for this — viewport is already handled inside `decoder.set_viewport()` / `decoder.pop_frame()`.

### Update watch example (`iroh-live/examples/watch.rs:217-224`)

`frame.img()` still works unchanged. The `dimensions()` call at line 218 changes from `frame.img().dimensions()` to `frame.dimensions()`. This avoids triggering a download for GPU frames.

### Files to modify
- `moq-media/src/format.rs` — new types, remove `image::Frame` from `DecodedVideoFrame`
- `moq-media/src/codec/h264/decoder.rs` — produce `CpuFrame`
- `moq-media/src/codec/av1/decoder.rs` — produce `CpuFrame`
- `moq-media/src/subscribe.rs` — guard BGRA swap
- `iroh-live/examples/watch.rs` — use `frame.dimensions()` instead of `frame.img().dimensions()`

### Verification
- `cargo build --workspace --all-features`
- `cargo test --workspace --all-features`
- `cargo clippy --workspace --all-features`
- All existing tests pass (they use `frame.img()` which still works)

---

## Phase 2: VAAPI H.264 Decoder (Linux)

### cros-codecs decoder API (verified in 0.0.6)

The C2Wrapper provides a high-level decode API. Key types:

```
StatelessDecoder::<H264, _>::new_vaapi(display, BlockingMode::NonBlocking)
  → impl StatelessVideoDecoder
    → fn decode(timestamp, bitstream, alloc_cb) → Result<usize, DecodeError>
    → fn next_event() → Option<DecoderEvent<Handle>>
    → fn flush() → Result<()>

DecoderEvent::FrameReady(handle) where handle: DecodedHandle
  → handle.sync()  — wait for decode completion
  → handle.video_frame() → Arc<V: VideoFrame>
  → handle.display_resolution() → Resolution
  → handle.timestamp() → u64
```

Frame allocation uses `FramePool` with `GbmVideoFrame` → `GenericDmaVideoFrame`:
```rust
let gbm_device = Arc::new(GbmDevice::open(PathBuf::from("/dev/dri/renderD128"))?);
let framepool = FramePool::new(move |stream_info: &StreamInfo| {
    gbm_device.new_frame(
        Fourcc::from(b"NV12"),
        stream_info.display_resolution,
        stream_info.coded_resolution,
        GbmUsage::Decode,
    )?.to_generic_dma_video_frame()?
});
```

`GenericDmaVideoFrame` holds DMA-BUF fds (via `Vec<File>`). It implements `VideoFrame` so can be read-mapped for CPU access. It's `Send + Sync`.

### Implementation: `moq-media/src/codec/vaapi/decoder.rs`

```rust
use cros_codecs::decoder::stateless::{StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::{DecoderEvent, BlockingMode};
use cros_codecs::video_frame::frame_pool::{FramePool, PooledVideoFrame};
use cros_codecs::video_frame::gbm_video_frame::{GbmDevice, GbmUsage};
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::video_frame::VideoFrame as CrosVideoFrame;

type VaapiFrame = PooledVideoFrame<GenericDmaVideoFrame>;

pub struct VaapiDecoder {
    decoder: Box<dyn StatelessVideoDecoder<Handle = dyn DecodedHandle<Frame = VaapiFrame>>>,
    // Actually: use DynStatelessVideoDecoder<VaapiFrame>
    framepool: Arc<Mutex<FramePool<GenericDmaVideoFrame>>>,
    pending_frames: VecDeque<DecodedVideoFrame>,
    clock: StreamClock,
    display: Rc<libva::Display>,
}
```

**Note**: `StatelessDecoder` uses `Rc<Display>` internally (not `Send`). The decoder runs on the WatchTrack OS thread which is single-threaded. The output `GenericDmaVideoFrame` IS `Send + Sync` since it holds owned `File` (DMA-BUF fds). So the `VaapiDecoder` itself can be `!Send` on the decode thread, but `DecodedVideoFrame` with `GpuFrame` wrapping the DMA-BUF data IS `Send`.

**Problem**: The `VideoDecoder` trait requires `Send + 'static`. But `Rc<Display>` is `!Send`. Solutions:
1. Wrap in an `unsafe impl Send` newtype (same pattern as `VaapiEncoder` — see encoder.rs:511: `unsafe impl Send for VaapiEncoder {}`)
2. Justification: single-threaded access guaranteed by WatchTrack's OS thread architecture

### VaapiGpuFrame

```rust
/// GPU-resident frame backed by DMA-BUF file descriptors.
/// Send + Sync because OwnedFd/File are Send + Sync.
#[derive(Debug)]
struct VaapiGpuFrame {
    frame: Arc<VaapiFrame>,  // Keeps DMA-BUF fds alive
    width: u32,
    height: u32,
}

impl GpuFrameInner for VaapiGpuFrame {
    fn download(&self) -> Result<CpuFrame> {
        // GenericDmaVideoFrame::map() → ReadMapping → get NV12 planes
        let mapping = self.frame.map().map_err(|e| anyhow!("{e}"))?;
        let planes = mapping.get();
        // planes[0] = Y, planes[1] = UV (NV12)
        // Convert NV12 → RGBA using yuvutils-rs (already a dep)
        let rgba = nv12_to_rgba(planes, self.width, self.height);
        Ok(CpuFrame { data: rgba, width: self.width, height: self.height, pixel_format: PixelFormat::Rgba })
    }

    fn gpu_pixel_format(&self) -> GpuPixelFormat { GpuPixelFormat::Nv12 }
    fn dimensions(&self) -> (u32, u32) { (self.width, self.height) }
}
```

### NV12→RGBA conversion helper

Add to `moq-media/src/processing/convert.rs`:
```rust
pub fn nv12_to_rgba(y_plane: &[u8], uv_plane: &[u8], width: u32, height: u32) -> Vec<u8> {
    // Use yuvutils-rs nv12_to_rgba function (already a dependency)
}
```

### Cargo.toml changes

cros-codecs already present. Need to add GBM dependency:
```toml
[target.'cfg(target_os = "linux")'.dependencies]
cros-codecs = { version = "0.0.6", optional = true, features = ["vaapi"] }
# gbm is pulled in transitively by cros-codecs via gbm-sys
```

Verify: `gbm-sys` is a transitive dep of cros-codecs with the vaapi feature. Check if we need explicit `drm-fourcc` dep.

### DynamicVideoDecoder update (`moq-media/src/codec/dynamic.rs`)

Add variant:
```rust
#[cfg(all(target_os = "linux", feature = "vaapi"))]
VaapiH264(super::vaapi::VaapiDecoder),
```

In `new()`, try HW first for H.264:
```rust
VideoCodec::H264(_) => {
    #[cfg(all(target_os = "linux", feature = "vaapi"))]
    if let Ok(dec) = super::vaapi::VaapiDecoder::new(config, playback_config) {
        return Ok(Self::VaapiH264(dec));
    }
    // ... fall through to software
}
```

### Files
- `moq-media/src/codec/vaapi/decoder.rs` — new
- `moq-media/src/codec/vaapi.rs` — add `mod decoder; pub use decoder::VaapiDecoder;`
- `moq-media/src/codec/dynamic.rs` — add variant + hw-first logic
- `moq-media/src/processing/convert.rs` — add `nv12_to_rgba`
- `moq-media/Cargo.toml` — possibly add drm-fourcc dep

### Verification
- `cargo build --workspace --features vaapi` on Linux with VA-API GPU
- Existing watch example should work (uses `img()` → triggers `download()` → NV12→RGBA)
- `cargo test --workspace` still passes

---

## Phase 3: VideoToolbox H.264 Decoder (macOS)

### VTDecompressionSession API (via objc2-video-toolbox 0.3)

Key flow:
1. Build `CMVideoFormatDescription` from avcC data (SPS/PPS) using `CMVideoFormatDescriptionCreateFromH264ParameterSets`
2. Create `VTDecompressionSession` with output pixel buffer attributes (request NV12 or BGRA output)
3. For each packet: build `CMSampleBuffer` from length-prefixed NAL data, call `VTDecompressionSessionDecodeFrame`
4. Callback fires synchronously with `CVPixelBuffer` containing decoded frame
5. `CVPixelBuffer` is backed by `IOSurface` when hardware-accelerated

### Implementation: `moq-media/src/codec/vtb/decoder.rs`

```rust
pub struct VtbDecoder {
    session: CFRetained<VTDecompressionSession>,
    pending: Vec<DecodedVideoFrame>,
    clock: StreamClock,
    format_desc: CFRetained<CMFormatDescription>,
}
```

Callback pattern — same as encoder (see vtb/encoder.rs:491-513):
```rust
type SharedFrameBuf = Arc<Mutex<Vec<VtbDecodedFrame>>>;

struct VtbDecodedFrame {
    pixel_buffer: CFRetained<CVPixelBuffer>,
    pts: Duration,
    width: u32,
    height: u32,
}

unsafe extern "C-unwind" fn decompression_output_callback(
    refcon: *mut c_void,
    _source_frame_refcon: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVPixelBuffer, // actually CVImageBufferRef
    pts: CMTime,
    _duration: CMTime,
) {
    // Same Arc pattern as encoder callback
    let state_ptr = refcon as *const Mutex<Vec<VtbDecodedFrame>>;
    Arc::increment_strong_count(state_ptr);
    let state = Arc::from_raw(state_ptr);

    let pixel_buffer = CFRetained::retain(NonNull::new(image_buffer).unwrap());
    let width = CVPixelBufferGetWidth(&pixel_buffer);
    let height = CVPixelBufferGetHeight(&pixel_buffer);

    state.lock().unwrap().push(VtbDecodedFrame {
        pixel_buffer, pts: cmtime_to_duration(pts), width, height
    });
}
```

### VtbGpuFrame

```rust
#[derive(Debug)]
struct VtbGpuFrame {
    pixel_buffer: CFRetained<CVPixelBuffer>,
    width: u32,
    height: u32,
}

// CFRetained<CVPixelBuffer> is Send (CF objects are ref-counted, thread-safe when retained)
unsafe impl Send for VtbGpuFrame {}
unsafe impl Sync for VtbGpuFrame {}

impl GpuFrameInner for VtbGpuFrame {
    fn download(&self) -> Result<CpuFrame> {
        unsafe {
            CVPixelBufferLockBaseAddress(&self.pixel_buffer, CVPixelBufferLockFlags(1)); // read-only
            // Read NV12 planes (or request BGRA output format from VTB for simpler download)
            let y = CVPixelBufferGetBaseAddressOfPlane(&self.pixel_buffer, 0);
            let uv = CVPixelBufferGetBaseAddressOfPlane(&self.pixel_buffer, 1);
            let y_stride = CVPixelBufferGetBytesPerRowOfPlane(&self.pixel_buffer, 0);
            let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(&self.pixel_buffer, 1);
            // Copy planes, convert NV12→RGBA
            let rgba = nv12_to_rgba_strided(y, uv, y_stride, uv_stride, self.width, self.height);
            CVPixelBufferUnlockBaseAddress(&self.pixel_buffer, CVPixelBufferLockFlags(1));
            Ok(CpuFrame { data: rgba, width: self.width, height: self.height, pixel_format: PixelFormat::Rgba })
        }
    }

    fn gpu_pixel_format(&self) -> GpuPixelFormat { GpuPixelFormat::Nv12 }
    fn dimensions(&self) -> (u32, u32) { (self.width, self.height) }
}
```

### Format description from avcC

Reuse existing `avcc_to_annex_b` / `extract_sps_pps` from `moq-media/src/codec/h264/annexb.rs`. Parse avcC to get raw SPS/PPS, then:
```rust
let sps_ptr = sps.as_ptr();
let pps_ptr = pps.as_ptr();
let param_sets = [sps_ptr, pps_ptr];
let param_sizes = [sps.len(), pps.len()];
CMVideoFormatDescriptionCreateFromH264ParameterSets(
    kCFAllocatorDefault,
    2,
    param_sets.as_ptr(),
    param_sizes.as_ptr(),
    4, // NAL length size
    &mut format_desc,
);
```

### Cargo.toml changes

Add `VTDecompressionSession` feature:
```toml
objc2-video-toolbox = { version = "0.3", optional = true, features = [
    "VTCompressionSession", "VTDecompressionSession", "VTErrors"
] }
```

### Files
- `moq-media/src/codec/vtb/decoder.rs` — new
- `moq-media/src/codec/vtb.rs` — add `mod decoder; pub use decoder::VtbDecoder;`
- `moq-media/src/codec/dynamic.rs` — add `VtbH264` variant
- `moq-media/Cargo.toml` — add VTDecompressionSession feature

### Verification
- `cargo build --workspace --features videotoolbox` on macOS
- Watch example works via `img()` → `download()` fallback
- `cargo test --workspace` passes

---

## Phase 4: wgpu Rendering Module

New `wgpu` feature in moq-media. New module `moq-media/src/render.rs`.

### WgpuVideoRenderer

```rust
/// Renders DecodedVideoFrames to a wgpu texture.
/// Zero-copy for GPU frames (NV12 shader), upload for CPU frames.
pub struct WgpuVideoRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    nv12_pipeline: wgpu::RenderPipeline,
    nv12_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    output_texture: Option<(wgpu::Texture, wgpu::TextureView)>,
    output_size: (u32, u32),
}

impl WgpuVideoRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self { ... }

    /// Render frame to an RGBA texture. Returns TextureView suitable for egui.
    pub fn render(&mut self, frame: &DecodedVideoFrame) -> &wgpu::TextureView {
        match &frame.buffer {
            FrameBuffer::Gpu(gpu) => self.render_gpu(gpu),
            FrameBuffer::Cpu(cpu) => self.render_cpu(cpu),
        }
    }

    fn render_cpu(&mut self, cpu: &CpuFrame) -> &wgpu::TextureView {
        // Ensure output texture matches frame size
        self.ensure_output_texture(cpu.width, cpu.height);
        // queue.write_texture() → RGBA data directly to output
        self.queue.write_texture(
            self.output_texture.as_ref().unwrap().0.as_image_copy(),
            &cpu.data,
            wgpu::TexelCopyBufferLayout { ... },
            wgpu::Extent3d { width: cpu.width, height: cpu.height, depth_or_array_layers: 1 },
        );
        &self.output_texture.as_ref().unwrap().1
    }

    fn render_gpu(&mut self, gpu: &GpuFrame) -> &wgpu::TextureView {
        let (w, h) = gpu.dimensions();
        self.ensure_output_texture(w, h);
        // Import NV12 planes as wgpu textures (platform-specific, see below)
        let imported = self.import_gpu_frame(gpu);
        // Run NV12→RGBA shader, output to self.output_texture
        self.run_nv12_shader(&imported);
        &self.output_texture.as_ref().unwrap().1
    }
}
```

### NV12→RGB WGSL shader

```wgsl
// nv12_to_rgb.wgsl
struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    // Fullscreen triangle
    var out: VertexOutput;
    let x = f32(i32(idx & 1u)) * 4.0 - 1.0;
    let y = f32(i32(idx >> 1u)) * 4.0 - 1.0;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@group(0) @binding(0) var y_tex: texture_2d<f32>;   // R8Unorm — Y plane
@group(0) @binding(1) var uv_tex: texture_2d<f32>;  // RG8Unorm — UV plane
@group(0) @binding(2) var samp: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let y = textureSample(y_tex, samp, in.uv).r;
    let u = textureSample(uv_tex, samp, in.uv).r - 0.5;
    let v = textureSample(uv_tex, samp, in.uv).g - 0.5;
    // BT.601 limited range
    let r = y + 1.402 * v;
    let g = y - 0.344136 * u - 0.714136 * v;
    let b = y + 1.772 * u;
    return vec4<f32>(clamp(r, 0.0, 1.0), clamp(g, 0.0, 1.0), clamp(b, 0.0, 1.0), 1.0);
}
```

### Platform texture import

Feature-gated `WgpuImportable` trait on `GpuFrameInner`:
```rust
#[cfg(feature = "wgpu")]
pub trait WgpuImportable {
    /// Import as wgpu textures (Y plane + UV plane for NV12).
    fn import_to_wgpu(&self, device: &wgpu::Device) -> Result<ImportedTextures>;
}

pub struct ImportedTextures {
    pub y_texture: wgpu::Texture,   // R8Unorm
    pub uv_texture: wgpu::Texture,  // RG8Unorm
    pub width: u32,
    pub height: u32,
}
```

**Linux (Vulkan)**: DMA-BUF fd → VkImage via `VK_EXT_external_memory_dma_buf`:
```rust
impl WgpuImportable for VaapiGpuFrame {
    fn import_to_wgpu(&self, device: &wgpu::Device) -> Result<ImportedTextures> {
        unsafe {
            let hal_device = device.as_hal::<wgpu::hal::vulkan::Api>().unwrap();
            // Create VkImage with VkExternalMemoryImageCreateInfo
            // Import DMA-BUF fd via VkImportMemoryFdInfoKHR
            // hal_device.texture_from_raw(vk_image, desc, Some(drop_guard))
            // device.create_texture_from_hal(hal_texture, desc)
        }
    }
}
```

**macOS (Metal)**: CVPixelBuffer → IOSurface → MTLTexture:
```rust
impl WgpuImportable for VtbGpuFrame {
    fn import_to_wgpu(&self, device: &wgpu::Device) -> Result<ImportedTextures> {
        unsafe {
            // CVPixelBufferGetIOSurface → IOSurfaceRef
            // MTLDevice.newTextureWithDescriptor:iosurface:plane:
            // hal_device.texture_from_raw(mtl_texture, format, ...)
            // device.create_texture_from_hal(hal_texture, desc)
        }
    }
}
```

### Cargo.toml

```toml
[dependencies]
wgpu = { version = "24", optional = true }

[features]
wgpu = ["dep:wgpu"]
```

Pin wgpu version — `as_hal()` / `create_texture_from_hal()` are semi-stable but version-dependent.

### Files
- `moq-media/src/render.rs` — new, `#[cfg(feature = "wgpu")]`
- `moq-media/src/render/nv12_to_rgb.wgsl` — NV12 shader
- `moq-media/src/lib.rs` — add `#[cfg(feature = "wgpu")] pub mod render;`
- `moq-media/Cargo.toml` — add wgpu dep + feature
- `moq-media/src/format.rs` — add `WgpuImportable` trait (cfg-gated)

### Verification
- `cargo build --workspace --features wgpu`
- Unit test: create WgpuVideoRenderer with wgpu::Device, render a CpuFrame, verify output texture

---

## Phase 5: Watch Example with `--wgpu`

### CLI option

```rust
#[derive(Debug, Parser)]
struct Cli {
    #[clap(long, conflicts_with = "endpoint-id")]
    ticket: Option<LiveTicket>,
    // ...
    /// Use wgpu for hardware-accelerated rendering (requires wgpu feature)
    #[cfg(feature = "wgpu")]
    #[clap(long)]
    wgpu: bool,
}
```

### eframe wgpu render state access

eframe 0.33 exposes via `CreationContext`:
```rust
// cc: &eframe::CreationContext
// cc.wgpu_render_state: Option<egui_wgpu::RenderState>
//   .device: wgpu::Device
//   .queue: wgpu::Queue
//   .renderer: Arc<RwLock<egui_wgpu::Renderer>>
```

`egui_wgpu::Renderer::register_native_texture()`:
```rust
renderer.register_native_texture(
    device: &wgpu::Device,
    texture: &wgpu::TextureView,   // must be Rgba8UnormSrgb
    texture_filter: wgpu::FilterMode,
) -> epaint::TextureId
```

Also: `update_egui_texture_from_wgpu_texture()` for reusing an existing TextureId.

### VideoView with wgpu

```rust
struct VideoView {
    track: WatchTrack,
    texture: egui::TextureHandle,
    size: egui::Vec2,
    #[cfg(feature = "wgpu")]
    wgpu_renderer: Option<WgpuVideoRenderer>,
    #[cfg(feature = "wgpu")]
    wgpu_texture_id: Option<epaint::TextureId>,
}

impl VideoView {
    fn render(&mut self, ctx: &egui::Context, available_size: Vec2) -> egui::Image<'_> {
        // ... viewport update same as before ...

        if let Some(frame) = self.track.current_frame() {
            #[cfg(feature = "wgpu")]
            if let Some(ref mut renderer) = self.wgpu_renderer {
                // Zero-copy GPU path
                let view = renderer.render(&frame);
                // Update egui texture from wgpu texture
                let render_state = ctx.data(|d| d.get_temp::<egui_wgpu::RenderState>(...));
                // Or: store RenderState in VideoView
                let mut egui_renderer = self.render_state.renderer.write();
                if let Some(id) = self.wgpu_texture_id {
                    egui_renderer.update_egui_texture_from_wgpu_texture(
                        &self.render_state.device, view, wgpu::FilterMode::Linear, id
                    );
                } else {
                    self.wgpu_texture_id = Some(egui_renderer.register_native_texture(
                        &self.render_state.device, view, wgpu::FilterMode::Linear
                    ));
                }
                return egui::Image::from_texture(
                    egui::load::SizedTexture::new(self.wgpu_texture_id.unwrap(), [avail.x, avail.y])
                );
            }

            // CPU fallback path (existing code)
            let (w, h) = frame.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize], frame.img().as_raw()
            );
            self.texture.set(image, Default::default());
        }
        egui::Image::from_texture(&self.texture).shrink_to_fit()
    }
}
```

### eframe NativeOptions

When `--wgpu` is passed, force eframe to use wgpu backend:
```rust
let native_options = if cli.wgpu {
    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
} else {
    eframe::NativeOptions::default()
};
```

### Cargo.toml

```toml
# iroh-live/Cargo.toml
[dev-dependencies]
eframe = { version = "0.33.0", features = ["wgpu"] }
egui-wgpu = "0.33.0"

[features]
wgpu = ["moq-media/wgpu"]
```

### Files
- `iroh-live/examples/watch.rs` — add `--wgpu` flag, wgpu rendering path
- `iroh-live/Cargo.toml` — add wgpu feature, eframe wgpu feature, egui-wgpu dep

### Verification
- `cargo run --example watch --features wgpu -- --wgpu --ticket <ticket>` on Linux with VAAPI
- `cargo run --example watch --features wgpu,videotoolbox -- --wgpu --ticket <ticket>` on macOS
- Without `--wgpu`: falls back to existing CPU path
- With `--wgpu` + SW decoder: CPU upload path works
- With `--wgpu` + HW decoder: zero-copy path, no CPU readback

---

## What Stays the Same
- `VideoDecoder` trait signature — unchanged
- `WatchTrack` / `WatchTrackFrames` channel architecture — unchanged
- `WatchTrackHandle` API — unchanged
- OS thread decode loop structure — small BGRA guard added
- Audio pipeline — completely unaffected
- Existing encoders — unaffected
- `SubscribeBroadcast` / `AvRemoteTrack` — unchanged
- `DynamicAudioDecoder` — unchanged

## Risks
- **wgpu-hal `as_hal()` stability**: Pin wgpu version. These APIs exist and are used by engines (bevy, rend3) but aren't guaranteed stable.
- **Surface pool exhaustion**: HW decoders have ~16-20 surfaces. `current_frame()` already drains old frames. Arc refcount on GpuFrame drops promptly.
- **cros-codecs FramePool + GbmDevice**: Requires `/dev/dri/renderD128`. Fail gracefully → fall back to software.
- **VTDecompressionSession thread model**: Callback fires synchronously on calling thread — this is fine since we call from the decode OS thread.

## Implementation Order

Each phase leaves everything compiling + tests passing:

1. **Phase 1** — frame abstraction (pure refactor)
2. **Phase 2** — VAAPI decoder (Linux, `--features vaapi`)
3. **Phase 3** — VTB decoder (macOS, `--features videotoolbox`)
4. **Phase 4** — wgpu renderer module (`--features wgpu`)
5. **Phase 5** — watch example with `--wgpu`
