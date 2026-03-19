//! Video frame renderers.
//!
//! - `wgpu` feature: [`WgpuVideoRenderer`] — renders to a wgpu RGBA texture.
//! - `gles` feature: [`gles::GlesRenderer`] — GLES2 fullscreen textured triangle.
//! - `dmabuf-import` feature: zero-copy Vulkan import from DMA-BUF.

#[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
pub mod dmabuf_import;

#[cfg(all(target_os = "linux", feature = "gles-dmabuf"))]
pub mod gles_dmabuf;

// TODO(CI1-macos): fix clippy lints in metal_import and remove this allow
#[allow(deprecated, unused_unsafe, reason = "objc2 API migration pending")]
#[cfg(all(target_os = "macos", feature = "metal-import"))]
pub mod metal_import;

#[cfg(feature = "gles")]
pub mod gles;

// Everything below is the wgpu renderer, gated on the `wgpu` feature.
#[cfg(feature = "wgpu")]
use std::{fmt, iter};

#[cfg(feature = "wgpu")]
use anyhow::{Context as _, Result};
#[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
pub use dmabuf_import::create_device_with_dmabuf_extensions;

#[cfg(any(
    all(target_os = "linux", feature = "dmabuf-import"),
    all(target_os = "macos", feature = "metal-import"),
))]
use crate::format::NativeFrameHandle;
#[cfg(feature = "wgpu")]
use crate::format::{FrameData, Nv12Planes, VideoFrame};

// ── wgpu renderer (feature = "wgpu") ────────────────────────────────

/// Renders decoded video frames to a wgpu RGBA texture.
///
/// - CPU frames: uploaded directly via `queue.write_texture()`
/// - GPU frames with DMA-BUF: zero-copy Vulkan import + NV12→RGBA shader
/// - GPU frames without DMA-BUF: NV12 plane download + upload + shader
#[cfg(feature = "wgpu")]
pub struct WgpuVideoRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    nv12_pipeline: wgpu::RenderPipeline,
    nv12_bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    output_texture: Option<OutputTexture>,
    /// Reusable NV12 plane textures (Y=R8, UV=RG8) to avoid per-frame allocation.
    nv12_planes: Option<Nv12PlaneTextures>,
    #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
    dmabuf_importer: Option<dmabuf_import::DmaBufImporter>,
    #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
    dmabuf_failures: u32,
    #[cfg(all(target_os = "macos", feature = "metal-import"))]
    metal_importer: Option<metal_import::MetalImporter>,
    #[cfg(all(target_os = "macos", feature = "metal-import"))]
    metal_failures: u32,
    /// Which render path was used for the last frame.
    last_render_path: RenderPath,
}

/// Describes which render path was used for the last frame.
#[cfg(feature = "wgpu")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderPath {
    /// No frame rendered yet.
    #[default]
    None,
    /// CPU RGBA packed upload.
    CpuRgba,
    /// CPU NV12 plane upload + GPU shader conversion.
    CpuNv12,
    /// Zero-copy DMA-BUF Vulkan import (Linux).
    DmaBuf,
    /// Zero-copy CVMetalTextureCache import (macOS).
    MetalZeroCopy,
    /// CPU RGBA download from GPU frame (fallback).
    GpuDownload,
}

#[cfg(feature = "wgpu")]
impl fmt::Display for RenderPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::CpuRgba => write!(f, "cpu-rgba"),
            Self::CpuNv12 => write!(f, "cpu-nv12"),
            Self::DmaBuf => write!(f, "dmabuf"),
            Self::MetalZeroCopy => write!(f, "metal-zerocopy"),
            Self::GpuDownload => write!(f, "gpu-download"),
        }
    }
}

#[cfg(feature = "wgpu")]
struct OutputTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

#[cfg(feature = "wgpu")]
struct Nv12PlaneTextures {
    y_texture: wgpu::Texture,
    uv_texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

#[cfg(feature = "wgpu")]
impl WgpuVideoRenderer {
    /// Create a new renderer from an existing wgpu device and queue.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12_to_rgba"),
            source: wgpu::ShaderSource::Wgsl(include_str!("nv12_to_rgba.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12_bind_group_layout"),
            entries: &[
                // Y texture (R8Unorm)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // UV texture (RG8Unorm)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Sampler
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nv12_to_rgba_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nv12_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
        let dmabuf_importer = dmabuf_import::DmaBufImporter::new(&device);
        #[cfg(all(target_os = "macos", feature = "metal-import"))]
        let metal_importer = metal_import::MetalImporter::new(&device);

        Self {
            device,
            queue,
            nv12_pipeline: pipeline,
            nv12_bind_group_layout: bind_group_layout,
            sampler,
            output_texture: None,
            nv12_planes: None,
            #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
            dmabuf_importer,
            #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
            dmabuf_failures: 0,
            #[cfg(all(target_os = "macos", feature = "metal-import"))]
            metal_importer,
            #[cfg(all(target_os = "macos", feature = "metal-import"))]
            metal_failures: 0,
            last_render_path: RenderPath::None,
        }
    }

    /// Returns which render path was used for the last frame.
    pub fn last_render_path(&self) -> RenderPath {
        self.last_render_path
    }

    /// Renders a frame to an RGBA texture. Returns a `TextureView` suitable for
    /// registering with egui via `register_native_texture`.
    pub fn render(&mut self, frame: &VideoFrame) -> Result<&wgpu::TextureView> {
        /// Disable DMA-BUF import after this many consecutive failures to avoid
        /// log spam and allocation churn from a fundamentally unsupported path.
        #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
        const MAX_DMABUF_FAILURES: u32 = 3;

        let [w, h] = frame.dimensions;
        match &frame.data {
            FrameData::Packed { data, .. } => {
                self.last_render_path = RenderPath::CpuRgba;
                self.render_packed(data, w, h)
            }
            FrameData::Nv12(planes) => {
                self.last_render_path = RenderPath::CpuNv12;
                self.render_nv12(planes)
            }
            FrameData::Gpu(gpu) => {
                // Try zero-copy DMA-BUF import (Linux only)
                #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
                if let Some(ref mut importer) = self.dmabuf_importer
                    && let Some(NativeFrameHandle::DmaBuf(ref info)) = gpu.native_handle()
                {
                    match importer.import_nv12(&self.device, info) {
                        Ok(imported) => {
                            if self.last_render_path != RenderPath::DmaBuf {
                                tracing::info!(
                                    modifier = format_args!("0x{:x}", info.modifier),
                                    "DMA-BUF zero-copy import active"
                                );
                            }
                            self.dmabuf_failures = 0;
                            self.last_render_path = RenderPath::DmaBuf;
                            return self.render_imported_nv12(imported);
                        }
                        Err(e) => {
                            self.dmabuf_failures += 1;
                            if self.dmabuf_failures >= MAX_DMABUF_FAILURES {
                                tracing::warn!(
                                    "DMA-BUF import failed {MAX_DMABUF_FAILURES} times, \
                                     disabling zero-copy path: {e}"
                                );
                                self.dmabuf_importer = None;
                            } else {
                                tracing::debug!("DMA-BUF import failed, falling back: {e}");
                            }
                        }
                    }
                }

                // Try zero-copy Metal import (macOS only)
                #[cfg(all(target_os = "macos", feature = "metal-import"))]
                if let Some(ref mut importer) = self.metal_importer
                    && let Some(NativeFrameHandle::CvPixelBuffer(ref info)) = gpu.native_handle()
                    && info.pixel_format == crate::format::GpuPixelFormat::Nv12
                {
                    match importer.import_nv12(&self.device, info) {
                        Ok(imported) => {
                            self.metal_failures = 0;
                            self.last_render_path = RenderPath::MetalZeroCopy;
                            return self.render_imported_metal_nv12(imported);
                        }
                        Err(e) => {
                            self.metal_failures += 1;
                            if self.metal_failures >= 3 {
                                tracing::warn!(
                                    "Metal import failed 3 times, disabling zero-copy: {e}"
                                );
                                self.metal_importer = None;
                            } else {
                                tracing::debug!("Metal import failed, falling back: {e}");
                            }
                        }
                    }
                }

                // Try NV12 plane upload + GPU shader conversion
                if let Some(Ok(planes)) = gpu.download_nv12() {
                    self.last_render_path = RenderPath::CpuNv12;
                    return self.render_nv12(&planes);
                }
                // Fallback: download as RGBA
                self.last_render_path = RenderPath::GpuDownload;
                let img = gpu.download_rgba().context("GPU frame download failed")?;
                self.render_packed(img.as_raw(), img.width(), img.height())
            }
            FrameData::I420 { .. } => {
                // Fall back through RGBA cache
                let img = frame.rgba_image();
                self.render_packed(img.as_raw(), img.width(), img.height())
            }
        }
    }

    /// Render imported DMA-BUF NV12 textures to RGBA via shader.
    #[cfg(all(target_os = "linux", feature = "dmabuf-import"))]
    fn render_imported_nv12(
        &mut self,
        imported: dmabuf_import::ImportedNv12Frame,
    ) -> Result<&wgpu::TextureView> {
        let w = imported.width;
        let h = imported.height;
        self.ensure_output_texture(w, h);

        let y_view = imported.y_texture.create_view(&Default::default());
        let uv_view = imported.uv_texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dmabuf_nv12_bind_group"),
            layout: &self.nv12_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let out = self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("dmabuf_nv12_to_rgba"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dmabuf_nv12_to_rgba_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.nv12_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(iter::once(encoder.finish()));

        Ok(&self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?
            .view)
    }

    /// Render zero-copy Metal-imported NV12 textures to RGBA via shader.
    #[cfg(all(target_os = "macos", feature = "metal-import"))]
    fn render_imported_metal_nv12(
        &mut self,
        imported: metal_import::ImportedMetalNv12,
    ) -> Result<&wgpu::TextureView> {
        let w = imported.width;
        let h = imported.height;
        self.ensure_output_texture(w, h);

        let y_view = imported.y_texture.create_view(&Default::default());
        let uv_view = imported.uv_texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("metal_nv12_bind_group"),
            layout: &self.nv12_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let out = self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("metal_nv12_to_rgba"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("metal_nv12_to_rgba_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.nv12_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(iter::once(encoder.finish()));

        Ok(&self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?
            .view)
    }

    /// Upload NV12 planes and render to RGBA via shader.
    fn render_nv12(&mut self, planes: &Nv12Planes) -> Result<&wgpu::TextureView> {
        let w = planes.width;
        let h = planes.height;
        self.ensure_output_texture(w, h);
        self.ensure_nv12_planes(w, h);

        let nv12 = self
            .nv12_planes
            .as_ref()
            .context("NV12 plane textures not initialized")?;

        // Upload Y plane data
        self.queue.write_texture(
            nv12.y_texture.as_image_copy(),
            &planes.y_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(planes.y_stride),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        // Upload UV plane data
        let uv_w = w / 2;
        let uv_h = h.div_ceil(2);
        self.queue.write_texture(
            nv12.uv_texture.as_image_copy(),
            &planes.uv_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(planes.uv_stride),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: uv_w,
                height: uv_h,
                depth_or_array_layers: 1,
            },
        );

        // Render fullscreen triangle to convert NV12 → RGBA
        let out = self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("nv12_to_rgba"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nv12_to_rgba_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.nv12_pipeline);
            pass.set_bind_group(0, &nv12.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(iter::once(encoder.finish()));

        Ok(&self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?
            .view)
    }

    /// Uploads packed RGBA pixel data to the output texture.
    fn render_packed(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<&wgpu::TextureView> {
        self.ensure_output_texture(width, height);

        let out = self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?;
        self.queue.write_texture(
            out.texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        Ok(&self
            .output_texture
            .as_ref()
            .context("output texture not initialized")?
            .view)
    }

    /// Returns the current output texture view, if any frame has been rendered.
    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.output_texture.as_ref().map(|t| &t.view)
    }

    /// Returns the current output texture, if any frame has been rendered.
    ///
    /// The texture format is [`wgpu::TextureFormat::Rgba8UnormSrgb`].
    /// This is useful for frameworks (e.g. dioxus-native) that need
    /// an owned `wgpu::Texture` for registration.
    pub fn output_texture(&self) -> Option<&wgpu::Texture> {
        self.output_texture.as_ref().map(|t| &t.texture)
    }

    /// Returns the current output dimensions `(width, height)`, if any frame has been rendered.
    pub fn output_dimensions(&self) -> Option<(u32, u32)> {
        self.output_texture.as_ref().map(|t| (t.width, t.height))
    }

    /// Get the NV12 pipeline and bind group layout for zero-copy GPU rendering.
    /// Used by platform-specific importers.
    pub fn nv12_pipeline(&self) -> (&wgpu::RenderPipeline, &wgpu::BindGroupLayout) {
        (&self.nv12_pipeline, &self.nv12_bind_group_layout)
    }

    /// Get a reference to the sampler.
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// Get a reference to the device.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Get a reference to the queue.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    fn ensure_nv12_planes(&mut self, width: u32, height: u32) {
        if let Some(ref nv12) = self.nv12_planes
            && nv12.width == width
            && nv12.height == height
        {
            return;
        }

        let uv_w = width / 2;
        let uv_h = height.div_ceil(2);

        let y_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nv12_y"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let y_view = y_texture.create_view(&Default::default());

        let uv_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nv12_uv"),
            size: wgpu::Extent3d {
                width: uv_w,
                height: uv_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let uv_view = uv_texture.create_view(&Default::default());

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12_bind_group"),
            layout: &self.nv12_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        self.nv12_planes = Some(Nv12PlaneTextures {
            y_texture,
            uv_texture,
            bind_group,
            width,
            height,
        });
    }

    fn ensure_output_texture(&mut self, width: u32, height: u32) {
        if let Some(out) = &self.output_texture
            && out.width == width
            && out.height == height
        {
            return;
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video_output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());

        self.output_texture = Some(OutputTexture {
            texture,
            view,
            width,
            height,
        });
    }
}

#[cfg(feature = "wgpu")]
impl fmt::Debug for WgpuVideoRenderer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WgpuVideoRenderer")
            .field(
                "output_size",
                &self.output_texture.as_ref().map(|t| (t.width, t.height)),
            )
            .finish()
    }
}
