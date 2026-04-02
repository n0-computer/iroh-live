//! Zero-copy DMA-BUF → wgpu texture import via raw Vulkan.
//!
//! Imports NV12 DMA-BUFs from VAAPI into wgpu textures using:
//! - `VK_EXT_image_drm_format_modifier` for tiling layout
//! - `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` for fd import
//!
//! The DMA-BUF is imported as a multi-plane NV12 VkImage
//! (`G8_B8R8_2PLANE_420_UNORM`), then each plane is GPU-copied to a separate
//! R8/RG8 VkImage that wgpu can sample.
//!
//! When the VAAPI decoder produces surfaces with a tiling modifier that Vulkan
//! cannot import (e.g. Y_TILED on Intel), a VAAPI VPP blit is used to re-tile
//! the surface to a Vulkan-compatible modifier before import.

use std::{ffi::CStr, fmt, io, os::fd::AsRawFd};

use ash::{ext::image_drm_format_modifier, vk};
use tracing::debug;
use wgpu::hal::MemoryFlags;

use crate::format::DmaBufInfo;

/// Extension name constant.
const VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME: &CStr = image_drm_format_modifier::NAME;

/// Imports DMA-BUF file descriptors as wgpu textures via raw Vulkan calls.
///
/// Requires the wgpu device to have been created with
/// `VK_EXT_image_drm_format_modifier` enabled (via `open_with_callback`).
pub struct DmaBufImporter {
    device: ash::Device,
    physical_device: vk::PhysicalDevice,
    instance: ash::Instance,
    queue: vk::Queue,
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    /// Persistent command buffer, reset and reused each frame.
    command_buffer: vk::CommandBuffer,
    /// Persistent fence, reset and reused each frame.
    fence: vk::Fence,
    /// Modifiers supported by Vulkan for the multi-plane NV12 format.
    supported_nv12_modifiers: Vec<u64>,
    /// Cached R8/RG8 copy-target textures, reused when dimensions match.
    cached_targets: Option<CachedCopyTargets>,
    /// Previous frame's NV12 import resources, kept alive until the fence
    /// signals at the start of the next import. This defers the GPU stall
    /// so the copy overlaps with the caller's CPU work.
    in_flight: Option<InFlightImport>,
    cached_nv12_import: Option<CachedNv12Import>,
    /// DRM render node path matching this Vulkan device (e.g. "/dev/dri/renderD128").
    /// Used by the VPP retiler to open the correct VA display.
    #[cfg(feature = "vaapi")]
    render_node_path: Option<String>,
    /// VAAPI VPP retiler for incompatible modifiers. Lazily initialized on
    /// first use. `None` means not yet attempted; `Some(None)` means init
    /// failed and we shouldn't retry.
    #[cfg(feature = "vaapi")]
    vpp_retiler: Option<Option<VppRetiler>>,
}

/// Resources from the previous frame's NV12 import that must stay alive
/// until the GPU copy completes (signaled by the fence).
struct InFlightImport {
    nv12_image: vk::Image,
    nv12_memories: Vec<vk::DeviceMemory>,
}

/// Cached NV12 VkImage import for buffer identity caching.
struct CachedNv12Import {
    nv12_image: vk::Image,
    nv12_memories: Vec<vk::DeviceMemory>,
    inode: u64,
    modifier: u64,
    coded_width: u32,
    coded_height: u32,
}

/// Cached Vulkan resources for the R8/RG8 copy targets, reused across frames.
struct CachedCopyTargets {
    y_image: vk::Image,
    y_memory: vk::DeviceMemory,
    uv_image: vk::Image,
    uv_memory: vk::DeviceMemory,
    y_width: u32,
    y_height: u32,
    uv_width: u32,
    uv_height: u32,
}

impl fmt::Debug for DmaBufImporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaBufImporter")
            .field("physical_device", &self.physical_device)
            .finish_non_exhaustive()
    }
}

/// Imported NV12 frame as separate Y and UV wgpu textures.
#[derive(Debug)]
pub struct ImportedNv12Frame {
    pub y_texture: wgpu::Texture,
    pub uv_texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
}

impl DmaBufImporter {
    /// Try to create a DMA-BUF importer from an existing wgpu device.
    ///
    /// Returns `None` if the device is not Vulkan-backed or
    /// `VK_EXT_image_drm_format_modifier` is not enabled.
    pub fn new(wgpu_device: &wgpu::Device) -> Option<Self> {
        use wgpu::hal::api::Vulkan as VkApi;

        unsafe {
            let hal_device_guard = wgpu_device.as_hal::<VkApi>()?;
            let hal_device = &*hal_device_guard;

            let has_drm_modifier = hal_device
                .enabled_device_extensions()
                .contains(&VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME);
            if !has_drm_modifier {
                debug!("VK_EXT_image_drm_format_modifier not enabled, DMA-BUF import unavailable");
                return None;
            }

            let raw_device = hal_device.raw_device().clone();
            let physical_device = hal_device.raw_physical_device();
            let instance = hal_device.shared_instance().raw_instance().clone();
            let queue = hal_device.raw_queue();
            let queue_family_index = hal_device.queue_family_index();

            // Log device name for diagnosing multi-GPU mismatches.
            let props = instance.get_physical_device_properties(physical_device);
            let device_name = CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy();

            // Query supported DRM modifiers for multi-plane NV12 format.
            let supported_nv12_modifiers = query_format_modifiers(
                &instance,
                physical_device,
                vk::Format::G8_B8R8_2PLANE_420_UNORM,
            );

            // Detect the DRM render node for this Vulkan device so the VPP
            // retiler opens the same GPU rather than hardcoding renderD128.
            #[cfg(feature = "vaapi")]
            let render_node_path = {
                let mut drm_props = vk::PhysicalDeviceDrmPropertiesEXT::default();
                let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut drm_props);
                instance.get_physical_device_properties2(physical_device, &mut props2);
                if drm_props.has_render != 0 {
                    let path = format!("/dev/dri/renderD{}", drm_props.render_minor);
                    debug!("Vulkan device render node: {path}");
                    Some(path)
                } else {
                    debug!("Vulkan device has no render node, VPP will probe");
                    None
                }
            };

            debug!(
                "DMA-BUF importer on {device_name}: NV12 modifiers={:?}",
                supported_nv12_modifiers
                    .iter()
                    .map(|m| format!("0x{m:x}"))
                    .collect::<Vec<_>>(),
            );

            // Create a command pool for GPU copy operations.
            let pool_info = vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family_index)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
            let command_pool = raw_device
                .create_command_pool(&pool_info, None)
                .inspect_err(|e| {
                    debug!("Failed to create command pool for DMA-BUF import: {e}");
                })
                .ok()?;

            // Allocate a persistent command buffer (reset and reused each frame).
            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let command_buffer = raw_device
                .allocate_command_buffers(&alloc_info)
                .inspect_err(|e| {
                    debug!("Failed to allocate command buffer for DMA-BUF import: {e}");
                    raw_device.destroy_command_pool(command_pool, None);
                })
                .ok()?[0];

            // Allocate a persistent fence (reset and reused each frame).
            let fence_info = vk::FenceCreateInfo::default();
            let fence = raw_device
                .create_fence(&fence_info, None)
                .inspect_err(|e| {
                    debug!("Failed to create fence for DMA-BUF import: {e}");
                    raw_device.free_command_buffers(command_pool, &[command_buffer]);
                    raw_device.destroy_command_pool(command_pool, None);
                })
                .ok()?;

            Some(Self {
                device: raw_device,
                physical_device,
                instance,
                queue,
                queue_family_index,
                command_pool,
                command_buffer,
                fence,
                supported_nv12_modifiers,
                cached_targets: None,
                in_flight: None,
                cached_nv12_import: None,
                #[cfg(feature = "vaapi")]
                render_node_path,
                #[cfg(feature = "vaapi")]
                vpp_retiler: None, // lazily initialized on first incompatible modifier
            })
        }
    }

    /// Import a DMA-BUF NV12 frame as separate Y and UV wgpu textures.
    ///
    /// Imports as a multi-plane NV12 VkImage, then GPU-copies each plane to a
    /// separate R8/RG8 VkImage that wgpu can sample.
    ///
    /// If the DMA-BUF modifier is not Vulkan-compatible, a VAAPI VPP blit is
    /// used to re-tile the surface before import.
    pub fn import_nv12(
        &mut self,
        wgpu_device: &wgpu::Device,
        info: &DmaBufInfo,
    ) -> anyhow::Result<ImportedNv12Frame> {
        if info.planes.len() < 2 {
            return Err(anyhow::anyhow!("NV12 DMA-BUF must have at least 2 planes"));
        }

        // If modifier is not Vulkan-compatible, try VPP re-tile (requires vaapi feature).
        #[cfg(feature = "vaapi")]
        let retiled: DmaBufInfo;
        let info = if self.supported_nv12_modifiers.contains(&info.modifier) {
            info
        } else {
            #[cfg(feature = "vaapi")]
            {
                let render_node = self.render_node_path.as_deref();
                let retiler_slot =
                    self.vpp_retiler
                        .get_or_insert_with(|| match VppRetiler::new(render_node) {
                            Ok(r) => {
                                debug!("Initialized VAAPI VPP retiler for modifier conversion");
                                Some(r)
                            }
                            Err(e) => {
                                warn!("Failed to init VPP retiler: {e}");
                                None
                            }
                        });
                let retiler = retiler_slot.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "DMA-BUF modifier 0x{:x} not Vulkan-compatible and VPP retiler unavailable",
                        info.modifier,
                    )
                })?;
                retiled = retiler.retile(info)?;
                if !self.supported_nv12_modifiers.contains(&retiled.modifier) {
                    return Err(anyhow::anyhow!(
                        "VPP re-tile produced modifier 0x{:x} still not Vulkan-compatible (supported: {:?})",
                        retiled.modifier,
                        self.supported_nv12_modifiers
                            .iter()
                            .map(|m| format!("0x{m:x}"))
                            .collect::<Vec<_>>(),
                    ));
                }
                &retiled
            }
            #[cfg(not(feature = "vaapi"))]
            return Err(anyhow::anyhow!(
                "DMA-BUF modifier 0x{:x} not supported for NV12 import (supported: {:?})",
                info.modifier,
                self.supported_nv12_modifiers
                    .iter()
                    .map(|m| format!("0x{m:x}"))
                    .collect::<Vec<_>>(),
            ));
        };

        let w = info.display_width;
        let h = info.display_height;

        // Wait for the previous frame's GPU copy to complete and reclaim its
        // NV12 import resources. This defers the stall: the GPU copy from the
        // last call ran in parallel with the caller's CPU work between frames.
        self.reclaim_in_flight();

        let fd_inode = fd_inode(info.fd.as_raw_fd());
        let cache_hit = fd_inode != 0
            && self.cached_nv12_import.as_ref().is_some_and(|c| {
                c.inode == fd_inode
                    && c.modifier == info.modifier
                    && c.coded_width == info.coded_width
                    && c.coded_height == info.coded_height
            });

        let nv12_image = if cache_hit {
            self.cached_nv12_import.as_ref().unwrap().nv12_image
        } else {
            if let Some(old) = self.cached_nv12_import.take() {
                self.in_flight = Some(InFlightImport {
                    nv12_image: old.nv12_image,
                    nv12_memories: old.nv12_memories,
                });
                self.reclaim_in_flight();
            }
            let (nv12_image, nv12_memories) = self.import_nv12_multiplane(info)?;
            self.cached_nv12_import = Some(CachedNv12Import {
                nv12_image,
                nv12_memories,
                inode: fd_inode,
                modifier: info.modifier,
                coded_width: info.coded_width,
                coded_height: info.coded_height,
            });
            nv12_image
        };

        let result = self.copy_planes_to_textures(wgpu_device, nv12_image, w, h);
        if result.is_err()
            && !cache_hit
            && let Some(bad) = self.cached_nv12_import.take()
        {
            unsafe {
                self.device.destroy_image(bad.nv12_image, None);
                for mem in bad.nv12_memories {
                    self.device.free_memory(mem, None);
                }
            }
        }
        result
    }

    /// Waits for the previous frame's GPU copy fence and destroys the
    /// in-flight NV12 import resources. Called at the start of each import.
    fn reclaim_in_flight(&mut self) {
        if let Some(prev) = self.in_flight.take() {
            unsafe {
                // Wait for the GPU copy submitted last frame. The copy is
                // typically already done by now, so this rarely stalls.
                let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
                self.device.destroy_image(prev.nv12_image, None);
                for mem in prev.nv12_memories {
                    self.device.free_memory(mem, None);
                }
            }
        }
    }

    /// Import DMA-BUF as a non-disjoint multi-plane NV12 VkImage.
    ///
    /// Uses a single memory binding for the entire image. The plane_layouts in
    /// `ImageDrmFormatModifierExplicitCreateInfoEXT` tell Vulkan where each
    /// plane starts within the DMA-BUF. This avoids issues with CCS modifiers
    /// that have more memory planes than format planes.
    fn import_nv12_multiplane(
        &self,
        info: &DmaBufInfo,
    ) -> anyhow::Result<(vk::Image, Vec<vk::DeviceMemory>)> {
        let plane_layouts: Vec<vk::SubresourceLayout> = info
            .planes
            .iter()
            .map(|p| {
                vk::SubresourceLayout::default()
                    .offset(p.offset as u64)
                    .row_pitch(p.pitch as u64)
                    .size(0)
            })
            .collect();

        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(info.modifier)
            .plane_layouts(&plane_layouts);

        let mut ext_mem_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::G8_B8R8_2PLANE_420_UNORM)
            .extent(vk::Extent3D {
                width: info.coded_width,
                height: info.coded_height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut modifier_info)
            .push_next(&mut ext_mem_info);

        unsafe {
            let image = self
                .device
                .create_image(&image_info, None)
                .map_err(|e| anyhow::anyhow!("vkCreateImage (NV12 multi-plane): {e}"))?;

            // Single memory binding for the whole image (non-disjoint).
            let mem_reqs = self.device.get_image_memory_requirements(image);

            // Dup fd — Vulkan takes ownership.
            let fd = libc::dup(info.fd.as_raw_fd());
            if fd < 0 {
                self.device.destroy_image(image, None);
                return Err(anyhow::anyhow!(
                    "dup DMA-BUF fd: {}",
                    io::Error::last_os_error()
                ));
            }

            // AMD VA-API drivers return size=0 in the PRIME descriptor. The
            // Vulkan memory requirements may also understate the buffer size
            // for imported DMA-BUFs. Use lseek to get the actual buffer size
            // from the kernel and take the maximum. Well-known workaround
            // also used by mpv/libplacebo and GStreamer.
            let alloc_size = {
                let lseek_size = libc::lseek(fd, 0, libc::SEEK_END);
                if lseek_size > 0 {
                    libc::lseek(fd, 0, libc::SEEK_SET);
                    mem_reqs.size.max(lseek_size as u64)
                } else {
                    mem_reqs.size
                }
            };

            let mut import_info = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(fd);

            let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(image);

            let memory_type_index = self
                .find_memory_type(mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::empty())
                .ok_or_else(|| {
                    libc::close(fd);
                    self.device.destroy_image(image, None);
                    anyhow::anyhow!("no suitable memory type for NV12 image")
                })?;

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(alloc_size)
                .memory_type_index(memory_type_index)
                .push_next(&mut import_info)
                .push_next(&mut dedicated_info);

            let memory = match self.device.allocate_memory(&alloc_info, None) {
                Ok(m) => m,
                Err(e) => {
                    libc::close(fd);
                    self.device.destroy_image(image, None);
                    return Err(anyhow::anyhow!("vkAllocateMemory (NV12): {e}"));
                }
            };

            if let Err(e) = self.device.bind_image_memory(image, memory, 0) {
                self.device.destroy_image(image, None);
                self.device.free_memory(memory, None);
                return Err(anyhow::anyhow!("vkBindImageMemory (NV12): {e}"));
            }

            Ok((image, vec![memory]))
        }
    }

    /// GPU-copy NV12 planes to cached R8/RG8 images and wrap as wgpu textures.
    ///
    /// Destination images are created at display size (not coded size) to avoid
    /// a mismatch between the VkImage extent and the wgpu texture descriptor.
    /// Only the display region is copied, cropping codec padding.
    ///
    /// The R8/RG8 VkImages and their memory are cached across frames when
    /// dimensions match, eliminating ~4 Vulkan create/destroy calls per frame.
    /// The wgpu texture wrappers are non-owning — the underlying Vulkan
    /// resources are owned by `cached_targets` and cleaned up in `Drop`.
    fn copy_planes_to_textures(
        &mut self,
        wgpu_device: &wgpu::Device,
        nv12_image: vk::Image,
        display_width: u32,
        display_height: u32,
    ) -> anyhow::Result<ImportedNv12Frame> {
        let uv_display_w = display_width / 2;
        let uv_display_h = display_height.div_ceil(2);

        // Ensure cached copy targets exist and match dimensions.
        let needs_recreate = match &self.cached_targets {
            Some(t) => {
                t.y_width != display_width
                    || t.y_height != display_height
                    || t.uv_width != uv_display_w
                    || t.uv_height != uv_display_h
            }
            None => true,
        };

        if needs_recreate {
            // Destroy old targets if any.
            if let Some(old) = self.cached_targets.take() {
                unsafe {
                    self.device.destroy_image(old.y_image, None);
                    self.device.free_memory(old.y_memory, None);
                    self.device.destroy_image(old.uv_image, None);
                    self.device.free_memory(old.uv_memory, None);
                }
            }

            unsafe {
                let y_image = self.create_optimal_image(
                    vk::Format::R8_UNORM,
                    display_width,
                    display_height,
                    vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
                )?;
                let y_memory = self.allocate_and_bind_image(y_image)?;

                let uv_image = match self.create_optimal_image(
                    vk::Format::R8G8_UNORM,
                    uv_display_w,
                    uv_display_h,
                    vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
                ) {
                    Ok(img) => img,
                    Err(e) => {
                        self.device.destroy_image(y_image, None);
                        self.device.free_memory(y_memory, None);
                        return Err(e);
                    }
                };
                let uv_memory = match self.allocate_and_bind_image(uv_image) {
                    Ok(m) => m,
                    Err(e) => {
                        self.device.destroy_image(y_image, None);
                        self.device.free_memory(y_memory, None);
                        self.device.destroy_image(uv_image, None);
                        return Err(e);
                    }
                };

                self.cached_targets = Some(CachedCopyTargets {
                    y_image,
                    y_memory,
                    uv_image,
                    uv_memory,
                    y_width: display_width,
                    y_height: display_height,
                    uv_width: uv_display_w,
                    uv_height: uv_display_h,
                });
            }
        }

        let targets = self.cached_targets.as_ref().unwrap();

        // GPU-copy planes into the cached targets.
        unsafe {
            self.record_and_submit_copy(
                nv12_image,
                targets.y_image,
                targets.uv_image,
                display_width,
                display_height,
                uv_display_w,
                uv_display_h,
            )?;
        }

        // Wrap as non-owning wgpu textures. The drop callback is a no-op
        // because the Vulkan resources are owned by cached_targets.
        let y_texture = self.wrap_as_wgpu_texture_non_owning(
            wgpu_device,
            targets.y_image,
            display_width,
            display_height,
            wgpu::TextureFormat::R8Unorm,
        )?;

        let uv_texture = self.wrap_as_wgpu_texture_non_owning(
            wgpu_device,
            targets.uv_image,
            uv_display_w,
            uv_display_h,
            wgpu::TextureFormat::Rg8Unorm,
        )?;

        Ok(ImportedNv12Frame {
            y_texture,
            uv_texture,
            width: display_width,
            height: display_height,
        })
    }

    /// Create a VkImage with OPTIMAL tiling.
    unsafe fn create_optimal_image(
        &self,
        format: vk::Format,
        width: u32,
        height: u32,
        usage: vk::ImageUsageFlags,
    ) -> anyhow::Result<vk::Image> {
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        unsafe {
            self.device
                .create_image(&image_info, None)
                .map_err(|e| anyhow::anyhow!("vkCreateImage ({format:?}): {e}"))
        }
    }

    /// Allocate device-local memory and bind it to an image.
    unsafe fn allocate_and_bind_image(&self, image: vk::Image) -> anyhow::Result<vk::DeviceMemory> {
        unsafe {
            let mem_reqs = self.device.get_image_memory_requirements(image);
            let memory_type_index = self
                .find_memory_type(
                    mem_reqs.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                )
                .or_else(|| {
                    self.find_memory_type(
                        mem_reqs.memory_type_bits,
                        vk::MemoryPropertyFlags::empty(),
                    )
                })
                .ok_or_else(|| anyhow::anyhow!("no suitable memory type for destination image"))?;

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(memory_type_index);

            let memory = self
                .device
                .allocate_memory(&alloc_info, None)
                .map_err(|e| anyhow::anyhow!("vkAllocateMemory (destination): {e}"))?;

            self.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| {
                    self.device.free_memory(memory, None);
                    anyhow::anyhow!("vkBindImageMemory (destination): {e}")
                })?;

            Ok(memory)
        }
    }

    /// Record and submit a command buffer that copies NV12 planes to R8/RG8 images.
    #[allow(
        clippy::too_many_arguments,
        reason = "Vulkan API requires many parameters"
    )]
    unsafe fn record_and_submit_copy(
        &self,
        nv12_image: vk::Image,
        y_image: vk::Image,
        uv_image: vk::Image,
        y_width: u32,
        y_height: u32,
        uv_width: u32,
        uv_height: u32,
    ) -> anyhow::Result<()> {
        unsafe {
            // Reset and reuse the persistent command buffer.
            self.device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|e| anyhow::anyhow!("vkResetCommandBuffer: {e}"))?;
            let cmd = self.command_buffer;

            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            self.device.begin_command_buffer(cmd, &begin_info)?;

            // Transition NV12 source: UNDEFINED → TRANSFER_SRC_OPTIMAL.
            // Use both plane aspects for the multi-plane image.
            let src_barrier = vk::ImageMemoryBarrier::default()
                .image(nv12_image)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                .dst_queue_family_index(self.queue_family_index)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::PLANE_0 | vk::ImageAspectFlags::PLANE_1,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            // Transition destinations: UNDEFINED → TRANSFER_DST_OPTIMAL.
            let y_dst_barrier = vk::ImageMemoryBarrier::default()
                .image(y_image)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let uv_dst_barrier = vk::ImageMemoryBarrier::default()
                .image(uv_image)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[src_barrier, y_dst_barrier, uv_dst_barrier],
            );

            // Copy Y plane (plane 0) → R8 image.
            let y_copy = vk::ImageCopy {
                src_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_0,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                src_offset: vk::Offset3D::default(),
                dst_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                dst_offset: vk::Offset3D::default(),
                extent: vk::Extent3D {
                    width: y_width,
                    height: y_height,
                    depth: 1,
                },
            };
            self.device.cmd_copy_image(
                cmd,
                nv12_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                y_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[y_copy],
            );

            // Copy UV plane (plane 1) → RG8 image.
            let uv_copy = vk::ImageCopy {
                src_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::PLANE_1,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                src_offset: vk::Offset3D::default(),
                dst_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                dst_offset: vk::Offset3D::default(),
                extent: vk::Extent3D {
                    width: uv_width,
                    height: uv_height,
                    depth: 1,
                },
            };
            self.device.cmd_copy_image(
                cmd,
                nv12_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                uv_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[uv_copy],
            );

            // Transition destinations: TRANSFER_DST → SHADER_READ_ONLY_OPTIMAL.
            let y_final_barrier = vk::ImageMemoryBarrier::default()
                .image(y_image)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let uv_final_barrier = vk::ImageMemoryBarrier::default()
                .image(uv_image)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            self.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[y_final_barrier, uv_final_barrier],
            );

            self.device.end_command_buffer(cmd)?;

            // Submit with the persistent fence. We do NOT wait here — the
            // NV12 source resources are kept alive in `in_flight` and reclaimed
            // at the start of the next import call, after the fence signals.
            // This lets the GPU copy overlap with the caller's CPU work.
            self.device
                .reset_fences(&[self.fence])
                .map_err(|e| anyhow::anyhow!("vkResetFences: {e}"))?;
            let cmd_bufs_to_submit = [cmd];
            let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_bufs_to_submit);
            self.device
                .queue_submit(self.queue, &[submit_info], self.fence)
                .map_err(|e| anyhow::anyhow!("vkQueueSubmit (plane copy): {e}"))?;
        }

        Ok(())
    }

    /// Wrap a VkImage as a wgpu texture without ownership transfer.
    ///
    /// The drop callback is a no-op — the caller retains ownership of the
    /// VkImage and VkDeviceMemory. The returned texture must not outlive the
    /// underlying Vulkan resources.
    fn wrap_as_wgpu_texture_non_owning(
        &self,
        wgpu_device: &wgpu::Device,
        image: vk::Image,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> anyhow::Result<wgpu::Texture> {
        use wgpu::hal::api::Vulkan as VkApi;

        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some(if format == wgpu::TextureFormat::R8Unorm {
                "dmabuf_y"
            } else {
                "dmabuf_uv"
            }),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUses::RESOURCE,
            memory_flags: MemoryFlags::empty(),
            view_formats: vec![],
        };

        // No-op drop callback — Vulkan resources are owned by cached_targets.
        let drop_callback: Box<dyn FnOnce() + Send + Sync> = Box::new(|| {});

        unsafe {
            let hal_device_guard = wgpu_device
                .as_hal::<VkApi>()
                .ok_or_else(|| anyhow::anyhow!("device is not Vulkan"))?;
            let hal_device = &*hal_device_guard;

            let hal_texture = hal_device.texture_from_raw(image, &hal_desc, Some(drop_callback));

            let wgpu_desc = wgpu::TextureDescriptor {
                label: hal_desc.label,
                size: hal_desc.size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            };

            drop(hal_device_guard);

            let wgpu_texture =
                wgpu_device.create_texture_from_hal::<VkApi>(hal_texture, &wgpu_desc);
            Ok(wgpu_texture)
        }
    }

    fn find_memory_type(
        &self,
        type_filter: u32,
        properties: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        let mem_properties = unsafe {
            self.instance
                .get_physical_device_memory_properties(self.physical_device)
        };
        (0..mem_properties.memory_type_count).find(|&i| {
            (type_filter & (1 << i)) != 0
                && mem_properties.memory_types[i as usize]
                    .property_flags
                    .contains(properties)
        })
    }
}

impl Drop for DmaBufImporter {
    fn drop(&mut self) {
        unsafe {
            self.reclaim_in_flight();
            if let Some(c) = self.cached_nv12_import.take() {
                self.device.destroy_image(c.nv12_image, None);
                for m in c.nv12_memories {
                    self.device.free_memory(m, None);
                }
            }
            if let Some(targets) = self.cached_targets.take() {
                self.device.destroy_image(targets.y_image, None);
                self.device.free_memory(targets.y_memory, None);
                self.device.destroy_image(targets.uv_image, None);
                self.device.free_memory(targets.uv_memory, None);
            }

            self.device.destroy_fence(self.fence, None);
            // Command buffer is freed implicitly when pool is destroyed.
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

/// Returns the inode number of the given fd, or 0 if `fstat` fails.
fn fd_inode(fd: std::os::fd::RawFd) -> u64 {
    unsafe {
        let mut stat: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut stat) == 0 {
            stat.st_ino
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------------------
// VAAPI VPP retiler — re-tiles DMA-BUFs to Vulkan-compatible modifiers
// ---------------------------------------------------------------------------

#[cfg(feature = "vaapi")]
use tracing::warn;

#[cfg(feature = "vaapi")]
use super::vpp_retiler::VppRetiler;

/// Query supported DRM format modifiers for a given Vulkan format.
fn query_format_modifiers(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    format: vk::Format,
) -> Vec<u64> {
    unsafe {
        let mut ml = vk::DrmFormatModifierPropertiesListEXT::default();
        let mut fp = vk::FormatProperties2::default().push_next(&mut ml);
        instance.get_physical_device_format_properties2(physical_device, format, &mut fp);
        let count = ml.drm_format_modifier_count as usize;
        if count == 0 {
            return Vec::new();
        }
        let mut props = vec![vk::DrmFormatModifierPropertiesEXT::default(); count];
        let mut ml2 = vk::DrmFormatModifierPropertiesListEXT::default()
            .drm_format_modifier_properties(&mut props);
        let mut fp2 = vk::FormatProperties2::default().push_next(&mut ml2);
        instance.get_physical_device_format_properties2(physical_device, format, &mut fp2);
        props.iter().map(|p| p.drm_format_modifier).collect()
    }
}

/// Create a wgpu device with `VK_EXT_image_drm_format_modifier` enabled.
///
/// Uses `open_with_callback` to inject the extension at device creation time,
/// then wraps the result via `create_device_from_hal`.
pub fn create_device_with_dmabuf_extensions(
    adapter: &wgpu::Adapter,
) -> anyhow::Result<(wgpu::Device, wgpu::Queue)> {
    use wgpu::hal::api::Vulkan as VkApi;

    unsafe {
        let hal_adapter_guard = adapter
            .as_hal::<VkApi>()
            .ok_or_else(|| anyhow::anyhow!("adapter is not Vulkan"))?;
        let hal_adapter = &*hal_adapter_guard;

        let open_device = hal_adapter
            .open_with_callback(
                wgpu::Features::default(),
                &wgpu::MemoryHints::default(),
                Some(Box::new(|args| {
                    args.extensions.push(VK_EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME);
                    debug!("Enabled VK_EXT_image_drm_format_modifier for DMA-BUF import");
                })),
            )
            .map_err(|e| {
                anyhow::anyhow!("Vulkan device creation with DRM modifier extension failed: {e:?}")
            })?;

        drop(hal_adapter_guard);

        let desc = wgpu::DeviceDescriptor {
            label: Some("iroh-live wgpu (dmabuf)"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: 8192,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        };

        adapter
            .create_device_from_hal(open_device, &desc)
            .map_err(|e| anyhow::anyhow!("create_device_from_hal failed: {e}"))
    }
}
