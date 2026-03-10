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
    /// Modifiers supported by Vulkan for the multi-plane NV12 format.
    supported_nv12_modifiers: Vec<u64>,
    /// VAAPI VPP retiler for incompatible modifiers. Lazily initialized on
    /// first use. `None` means not yet attempted; `Some(None)` means init
    /// failed and we shouldn't retry.
    #[cfg(feature = "vaapi")]
    vpp_retiler: Option<Option<VppRetiler>>,
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

            Some(Self {
                device: raw_device,
                physical_device,
                instance,
                queue,
                queue_family_index,
                command_pool,
                supported_nv12_modifiers,
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
                let retiler_slot =
                    self.vpp_retiler
                        .get_or_insert_with(|| match VppRetiler::new() {
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

        // Step 1: Import as multi-plane NV12 VkImage.
        let (nv12_image, nv12_memories) = self.import_nv12_multiplane(info)?;

        // Step 2: Create R8/RG8 destination images and GPU-copy planes.
        let result = self.copy_planes_to_textures(
            wgpu_device,
            nv12_image,
            info.coded_width,
            info.coded_height,
            w,
            h,
        );

        // Clean up the NV12 source image (we only needed it for the copy).
        unsafe {
            self.device.destroy_image(nv12_image, None);
            for mem in nv12_memories {
                self.device.free_memory(mem, None);
            }
        }

        result
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
                .allocation_size(mem_reqs.size)
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

    /// Create R8/RG8 destination textures and GPU-copy planes from the NV12 source.
    ///
    /// Destination images are created at display size (not coded size) to avoid
    /// a mismatch between the VkImage extent and the wgpu texture descriptor.
    /// Only the display region is copied, cropping codec padding.
    fn copy_planes_to_textures(
        &self,
        wgpu_device: &wgpu::Device,
        nv12_image: vk::Image,
        _coded_width: u32,
        _coded_height: u32,
        display_width: u32,
        display_height: u32,
    ) -> anyhow::Result<ImportedNv12Frame> {
        let uv_display_w = display_width / 2;
        let uv_display_h = display_height.div_ceil(2);

        unsafe {
            // Create destination Y plane (R8) at display size.
            let y_image = self.create_optimal_image(
                vk::Format::R8_UNORM,
                display_width,
                display_height,
                vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            )?;
            let y_memory = self.allocate_and_bind_image(y_image)?;

            // Create destination UV plane (RG8) at display size.
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

            // Copy only the display region (crop codec padding).
            if let Err(e) = self.record_and_submit_copy(
                nv12_image,
                y_image,
                uv_image,
                display_width,
                display_height,
                uv_display_w,
                uv_display_h,
            ) {
                self.device.destroy_image(y_image, None);
                self.device.free_memory(y_memory, None);
                self.device.destroy_image(uv_image, None);
                self.device.free_memory(uv_memory, None);
                return Err(e);
            }

            // Wrap as wgpu textures — sizes now match the VkImage dimensions.
            let y_texture = self.wrap_as_wgpu_texture(
                wgpu_device,
                y_image,
                y_memory,
                display_width,
                display_height,
                wgpu::TextureFormat::R8Unorm,
            )?;

            let uv_texture = match self.wrap_as_wgpu_texture(
                wgpu_device,
                uv_image,
                uv_memory,
                uv_display_w,
                uv_display_h,
                wgpu::TextureFormat::Rg8Unorm,
            ) {
                Ok(t) => t,
                Err(e) => {
                    // y_texture owns y_image/y_memory now, it will be dropped.
                    // But uv_image/uv_memory need cleanup.
                    self.device.destroy_image(uv_image, None);
                    self.device.free_memory(uv_memory, None);
                    return Err(e);
                }
            };

            Ok(ImportedNv12Frame {
                y_texture,
                uv_texture,
                width: display_width,
                height: display_height,
            })
        }
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
            // Allocate a one-shot command buffer.
            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(self.command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);

            let cmd_bufs = self
                .device
                .allocate_command_buffers(&alloc_info)
                .map_err(|e| anyhow::anyhow!("vkAllocateCommandBuffers: {e}"))?;
            let cmd = cmd_bufs[0];

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

            // Submit and wait. We must wait because the NV12 source image will be
            // destroyed after this call returns.
            let cmd_bufs_to_submit = [cmd];
            let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_bufs_to_submit);
            self.device
                .queue_submit(self.queue, &[submit_info], vk::Fence::null())
                .map_err(|e| anyhow::anyhow!("vkQueueSubmit (plane copy): {e}"))?;
            self.device
                .queue_wait_idle(self.queue)
                .map_err(|e| anyhow::anyhow!("vkQueueWaitIdle: {e}"))?;

            self.device
                .free_command_buffers(self.command_pool, &cmd_bufs_to_submit);
        }

        Ok(())
    }

    fn wrap_as_wgpu_texture(
        &self,
        wgpu_device: &wgpu::Device,
        image: vk::Image,
        memory: vk::DeviceMemory,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> anyhow::Result<wgpu::Texture> {
        use wgpu::hal::api::Vulkan as VkApi;

        let device_clone = self.device.clone();

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

        let drop_callback: Box<dyn FnOnce() + Send + Sync> = Box::new(move || unsafe {
            device_clone.destroy_image(image, None);
            device_clone.free_memory(memory, None);
        });

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
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

// ---------------------------------------------------------------------------
// VAAPI VPP retiler — re-tiles DMA-BUFs to Vulkan-compatible modifiers
// ---------------------------------------------------------------------------

#[cfg(feature = "vaapi")]
use std::fs::File;
#[cfg(feature = "vaapi")]
use std::os::fd::{FromRawFd, OwnedFd};

#[cfg(feature = "vaapi")]
use cros_codecs::libva as va;
#[cfg(feature = "vaapi")]
use tracing::warn;

#[cfg(feature = "vaapi")]
use crate::format::DmaBufPlaneInfo;

/// VAProcPipelineParameterBuffer from va/va_vpp.h.
///
/// Not included in cros-libva's generated bindings. For an identity blit
/// (format/tiling conversion), only `surface` needs to be set.
#[cfg(feature = "vaapi")]
#[repr(C)]
struct VaProcPipelineParameterBuffer {
    surface: va::VASurfaceID,
    surface_region: *const va::VARectangle,
    surface_color_standard: u32,
    output_region: *const va::VARectangle,
    output_background_color: u32,
    output_color_standard: u32,
    pipeline_flags: u32,
    filter_flags: u32,
    filters: *mut va::VABufferID,
    num_filters: u32,
    forward_references: *mut va::VASurfaceID,
    num_forward_references: u32,
    backward_references: *mut va::VASurfaceID,
    num_backward_references: u32,
    rotation_state: u32,
    blend_state: *const std::ffi::c_void,
    mirror_state: u32,
    additional_outputs: *mut va::VASurfaceID,
    num_additional_outputs: u32,
    input_surface_flag: u32,
    output_surface_flag: u32,
    _pad: [u8; 256],
}

/// VAAPI Video Post Processing retiler.
///
/// Uses a VPP identity blit to convert decoded surfaces from an incompatible
/// tiling modifier (e.g. Y_TILED) to one that Vulkan can import. Caches the
/// raw VA display and VPP config across frames.
#[cfg(feature = "vaapi")]
struct VppRetiler {
    dpy: va::VADisplay,
    config_id: va::VAConfigID,
    _drm_file: File,
}

#[cfg(feature = "vaapi")]
impl VppRetiler {
    fn new() -> anyhow::Result<Self> {
        unsafe {
            for path in ["/dev/dri/renderD128", "/dev/dri/renderD129"] {
                if let Ok(file) = File::options().read(true).write(true).open(path) {
                    let dpy = va::vaGetDisplayDRM(file.as_raw_fd());
                    if dpy.is_null() {
                        continue;
                    }
                    let mut major = 0i32;
                    let mut minor = 0i32;
                    let st = va::vaInitialize(dpy, &mut major, &mut minor);
                    if st != va::VA_STATUS_SUCCESS as i32 {
                        continue;
                    }

                    let mut rt_attr = va::VAConfigAttrib {
                        type_: va::VAConfigAttribType::VAConfigAttribRTFormat,
                        value: 0,
                    };
                    va_check(
                        va::vaGetConfigAttributes(
                            dpy,
                            va::VAProfile::VAProfileNone,
                            va::VAEntrypoint::VAEntrypointVideoProc,
                            &mut rt_attr,
                            1,
                        ),
                        "vaGetConfigAttributes(VPP)",
                    )?;

                    let mut config_id: va::VAConfigID = 0;
                    va_check(
                        va::vaCreateConfig(
                            dpy,
                            va::VAProfile::VAProfileNone,
                            va::VAEntrypoint::VAEntrypointVideoProc,
                            &mut rt_attr,
                            1,
                            &mut config_id,
                        ),
                        "vaCreateConfig(VPP)",
                    )?;

                    debug!("VPP retiler initialized on {path}");
                    return Ok(Self {
                        dpy,
                        config_id,
                        _drm_file: file,
                    });
                }
            }
            Err(anyhow::anyhow!("no VA display found for VPP retiler"))
        }
    }

    /// Re-tile a DMA-BUF surface via VPP identity blit.
    fn retile(&self, info: &DmaBufInfo) -> anyhow::Result<DmaBufInfo> {
        unsafe {
            // Import decoded DMA-BUF as VA surface using DRM_PRIME_2 (preserves modifier).
            let fd_dup = libc::dup(info.fd.as_raw_fd());
            if fd_dup < 0 {
                return Err(anyhow::anyhow!(
                    "dup input fd: {}",
                    io::Error::last_os_error()
                ));
            }

            let mut input_surface: va::VASurfaceID = 0;
            {
                // Build a PRIME2 descriptor matching the decoded surface layout.
                let mut prime_desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
                prime_desc.fourcc = info.drm_format;
                prime_desc.width = info.coded_width;
                prime_desc.height = info.coded_height;
                prime_desc.num_objects = 1;
                prime_desc.objects[0].fd = fd_dup;
                prime_desc.objects[0].size = 0; // driver calculates
                prime_desc.objects[0].drm_format_modifier = info.modifier;
                prime_desc.num_layers = 1;
                prime_desc.layers[0].drm_format = info.drm_format;
                prime_desc.layers[0].num_planes = info.planes.len() as u32;
                for (i, plane) in info.planes.iter().enumerate() {
                    prime_desc.layers[0].object_index[i] = 0;
                    prime_desc.layers[0].offset[i] = plane.offset;
                    prime_desc.layers[0].pitch[i] = plane.pitch;
                }

                let mut attribs: [va::VASurfaceAttrib; 2] = std::mem::zeroed();
                // Memory type: DRM_PRIME_2 (carries modifier info).
                attribs[0].type_ = va::VASurfaceAttribType::VASurfaceAttribMemoryType;
                attribs[0].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[0].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[0].value.value.i = va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32;
                // External buffer descriptor (PRIME2).
                attribs[1].type_ = va::VASurfaceAttribType::VASurfaceAttribExternalBufferDescriptor;
                attribs[1].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[1].value.type_ = va::VAGenericValueType::VAGenericValueTypePointer;
                attribs[1].value.value.p = &mut prime_desc as *mut _ as *mut std::ffi::c_void;

                let status = va::vaCreateSurfaces(
                    self.dpy,
                    va::VA_RT_FORMAT_YUV420,
                    info.coded_width,
                    info.coded_height,
                    &mut input_surface,
                    1,
                    attribs.as_mut_ptr(),
                    attribs.len() as u32,
                );
                if status != va::VA_STATUS_SUCCESS as i32 {
                    // Driver did not take ownership of fd_dup on failure.
                    libc::close(fd_dup);
                    return Err(anyhow::anyhow!(
                        "vaCreateSurfaces(VPP input) failed: VA status {status}"
                    ));
                }
            }

            // Create output surface with VPP_WRITE + EXPORT hints.
            let mut output_surface: va::VASurfaceID = 0;
            {
                let mut attribs: [va::VASurfaceAttrib; 2] = std::mem::zeroed();
                attribs[0].type_ = va::VASurfaceAttribType::VASurfaceAttribPixelFormat;
                attribs[0].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[0].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[0].value.value.i = va::VA_FOURCC_NV12 as i32;
                attribs[1].type_ = va::VASurfaceAttribType::VASurfaceAttribUsageHint;
                attribs[1].flags = va::VA_SURFACE_ATTRIB_SETTABLE;
                attribs[1].value.type_ = va::VAGenericValueType::VAGenericValueTypeInteger;
                attribs[1].value.value.i = (va::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_WRITE
                    | va::VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT)
                    as i32;

                va_check(
                    va::vaCreateSurfaces(
                        self.dpy,
                        va::VA_RT_FORMAT_YUV420,
                        info.coded_width,
                        info.coded_height,
                        &mut output_surface,
                        1,
                        attribs.as_mut_ptr(),
                        attribs.len() as u32,
                    ),
                    "vaCreateSurfaces(VPP output)",
                )?;
            }

            // Create VPP context.
            let mut context_id: va::VAContextID = 0;
            va_check(
                va::vaCreateContext(
                    self.dpy,
                    self.config_id,
                    info.coded_width as i32,
                    info.coded_height as i32,
                    0,
                    &mut output_surface,
                    1,
                    &mut context_id,
                ),
                "vaCreateContext(VPP)",
            )?;

            // Create pipeline parameter buffer (identity blit).
            let mut pipeline_param: VaProcPipelineParameterBuffer = std::mem::zeroed();
            pipeline_param.surface = input_surface;

            let mut buf_id: va::VABufferID = 0;
            va_check(
                va::vaCreateBuffer(
                    self.dpy,
                    context_id,
                    va::VABufferType::VAProcPipelineParameterBufferType,
                    std::mem::size_of::<VaProcPipelineParameterBuffer>() as u32,
                    1,
                    &mut pipeline_param as *mut _ as *mut std::ffi::c_void,
                    &mut buf_id,
                ),
                "vaCreateBuffer(VPP)",
            )?;

            // Execute VPP pipeline.
            va_check(
                va::vaBeginPicture(self.dpy, context_id, output_surface),
                "vaBeginPicture(VPP)",
            )?;
            va_check(
                va::vaRenderPicture(self.dpy, context_id, &mut buf_id, 1),
                "vaRenderPicture(VPP)",
            )?;
            va_check(va::vaEndPicture(self.dpy, context_id), "vaEndPicture(VPP)")?;
            va_check(
                va::vaSyncSurface(self.dpy, output_surface),
                "vaSyncSurface(VPP)",
            )?;

            // Export output surface as PRIME2.
            let mut desc: va::VADRMPRIMESurfaceDescriptor = std::mem::zeroed();
            va_check(
                va::vaExportSurfaceHandle(
                    self.dpy,
                    output_surface,
                    va::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                    va::VA_EXPORT_SURFACE_READ_ONLY | va::VA_EXPORT_SURFACE_COMPOSED_LAYERS,
                    &mut desc as *mut _ as *mut std::ffi::c_void,
                ),
                "vaExportSurfaceHandle(VPP output)",
            )?;

            // Take ownership of the primary object FD. Close any additional
            // object FDs (e.g. CCS aux planes) to prevent per-frame FD leaks.
            let primary_fd = OwnedFd::from_raw_fd(desc.objects[0].fd);
            for i in 1..desc.num_objects as usize {
                libc::close(desc.objects[i].fd);
            }

            let layer = &desc.layers[0];

            let result = DmaBufInfo {
                fd: primary_fd,
                modifier: desc.objects[0].drm_format_modifier,
                drm_format: desc.fourcc,
                coded_width: desc.width,
                coded_height: desc.height,
                display_width: info.display_width,
                display_height: info.display_height,
                planes: (0..layer.num_planes as usize)
                    .map(|i| DmaBufPlaneInfo {
                        offset: layer.offset[i],
                        pitch: layer.pitch[i],
                    })
                    .collect(),
            };

            // Cleanup VA resources (exported FD keeps the buffer alive).
            va::vaDestroyBuffer(self.dpy, buf_id);
            va::vaDestroyContext(self.dpy, context_id);
            va::vaDestroySurfaces(self.dpy, &mut input_surface, 1);
            va::vaDestroySurfaces(self.dpy, &mut output_surface, 1);

            Ok(result)
        }
    }
}

#[cfg(feature = "vaapi")]
impl Drop for VppRetiler {
    fn drop(&mut self) {
        unsafe {
            va::vaDestroyConfig(self.dpy, self.config_id);
            va::vaTerminate(self.dpy);
        }
    }
}

/// Check a VAStatus and return an error if not VA_STATUS_SUCCESS.
#[cfg(feature = "vaapi")]
fn va_check(status: va::VAStatus, op: &str) -> anyhow::Result<()> {
    if status != va::VA_STATUS_SUCCESS as i32 {
        Err(anyhow::anyhow!("{op} failed: VA status {status}"))
    } else {
        Ok(())
    }
}

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
