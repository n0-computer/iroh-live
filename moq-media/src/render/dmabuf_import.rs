//! Zero-copy DMA-BUF → wgpu texture import via raw Vulkan.
//!
//! Imports NV12 DMA-BUFs from VAAPI into wgpu textures using:
//! - `VK_EXT_image_drm_format_modifier` for tiling layout
//! - `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` for fd import
//!
//! The imported multi-planar NV12 VkImage is split into separate Y (R8) and
//! UV (RG8) textures via GPU copy, then fed into the existing NV12→RGBA shader.

use std::ffi::CStr;
use std::fmt;
use std::io;
use std::os::fd::AsRawFd;

use ash::vk;
use tracing::debug;

use ash::ext::image_drm_format_modifier;
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
    command_pool: vk::CommandPool,
    queue_family_index: u32,
    queue: vk::Queue,
}

impl fmt::Debug for DmaBufImporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaBufImporter")
            .field("physical_device", &self.physical_device)
            .field("queue_family_index", &self.queue_family_index)
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
            let queue = hal_device.raw_queue();
            let queue_family_index = hal_device.queue_family_index();
            let instance = hal_device.shared_instance().raw_instance().clone();

            let pool_info = vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family_index)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
            let command_pool = raw_device.create_command_pool(&pool_info, None).ok()?;

            debug!("DMA-BUF importer initialized");

            Some(Self {
                device: raw_device,
                physical_device,
                instance,
                command_pool,
                queue_family_index,
                queue,
            })
        }
    }

    /// Import a DMA-BUF NV12 frame as separate Y and UV wgpu textures.
    pub fn import_nv12(
        &self,
        wgpu_device: &wgpu::Device,
        info: &DmaBufInfo,
    ) -> anyhow::Result<ImportedNv12Frame> {
        let w = info.display_width;
        let h = info.display_height;
        let uv_w = info.coded_width / 2;
        let uv_h = info.coded_height / 2;

        // Dup the fd — Vulkan takes ownership on successful import.
        let fd = info.fd.as_raw_fd();
        let fd_for_import = unsafe { libc::dup(fd) };
        if fd_for_import < 0 {
            return Err(anyhow::anyhow!(
                "dup DMA-BUF fd: {}",
                io::Error::last_os_error()
            ));
        }

        // Step 1: Create multi-planar NV12 VkImage with DRM format modifier
        let nv12_image = match self.create_nv12_image(info) {
            Ok(img) => img,
            Err(e) => {
                unsafe { libc::close(fd_for_import) };
                return Err(e);
            }
        };

        // Step 2: Import DMA-BUF fd as VkDeviceMemory and bind to image.
        // On success, Vulkan takes ownership of fd_for_import.
        // On failure, Vulkan does NOT take ownership — close it ourselves.
        let nv12_memory = match self.import_and_bind_memory(nv12_image, fd_for_import) {
            Ok(mem) => mem,
            Err(e) => {
                unsafe {
                    libc::close(fd_for_import);
                    self.device.destroy_image(nv12_image, None);
                }
                return Err(e);
            }
        };

        // Step 3: Create destination R8 (Y) and RG8 (UV) images
        let (y_image, y_memory) = match self.create_plane_image(
            vk::Format::R8_UNORM,
            info.coded_width,
            info.coded_height,
        ) {
            Ok(v) => v,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(nv12_image, None);
                    self.device.free_memory(nv12_memory, None);
                }
                return Err(e);
            }
        };
        let (uv_image, uv_memory) =
            match self.create_plane_image(vk::Format::R8G8_UNORM, uv_w, uv_h) {
                Ok(v) => v,
                Err(e) => {
                    unsafe {
                        self.device.destroy_image(nv12_image, None);
                        self.device.free_memory(nv12_memory, None);
                        self.device.destroy_image(y_image, None);
                        self.device.free_memory(y_memory, None);
                    }
                    return Err(e);
                }
            };

        // Step 4: GPU copy planes from NV12 image to separate Y/UV images
        if let Err(e) = self.copy_planes(
            nv12_image,
            y_image,
            uv_image,
            info.coded_width,
            info.coded_height,
            uv_w,
            uv_h,
        ) {
            unsafe {
                self.device.destroy_image(nv12_image, None);
                self.device.free_memory(nv12_memory, None);
                self.device.destroy_image(y_image, None);
                self.device.free_memory(y_memory, None);
                self.device.destroy_image(uv_image, None);
                self.device.free_memory(uv_memory, None);
            }
            return Err(e);
        }

        // Step 5: Wrap Y and UV as wgpu textures
        // Y texture owns cleanup of the NV12 source image
        let y_texture = self.wrap_as_wgpu_texture(
            wgpu_device,
            y_image,
            y_memory,
            Some((nv12_image, nv12_memory)),
            w,
            h,
            wgpu::TextureFormat::R8Unorm,
        )?;
        let uv_texture = self.wrap_as_wgpu_texture(
            wgpu_device,
            uv_image,
            uv_memory,
            None,
            uv_w,
            uv_h,
            wgpu::TextureFormat::Rg8Unorm,
        )?;

        Ok(ImportedNv12Frame {
            y_texture,
            uv_texture,
            width: w,
            height: h,
        })
    }

    fn create_nv12_image(&self, info: &DmaBufInfo) -> anyhow::Result<vk::Image> {
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
            .usage(vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut modifier_info)
            .push_next(&mut ext_mem_info);

        unsafe {
            self.device
                .create_image(&image_info, None)
                .map_err(|e| anyhow::anyhow!("vkCreateImage (NV12 DMA-BUF): {e}"))
        }
    }

    fn import_and_bind_memory(
        &self,
        image: vk::Image,
        fd: i32,
    ) -> anyhow::Result<vk::DeviceMemory> {
        unsafe {
            let mem_reqs = self.device.get_image_memory_requirements(image);

            let mut import_info = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(fd);

            let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(image);

            let memory_type_index = self
                .find_memory_type(mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::empty())
                .ok_or_else(|| anyhow::anyhow!("no suitable memory type for DMA-BUF import"))?;

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(memory_type_index)
                .push_next(&mut import_info)
                .push_next(&mut dedicated_info);

            let memory = self
                .device
                .allocate_memory(&alloc_info, None)
                .map_err(|e| anyhow::anyhow!("vkAllocateMemory (DMA-BUF import): {e}"))?;

            self.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| anyhow::anyhow!("vkBindImageMemory (DMA-BUF): {e}"))?;

            Ok(memory)
        }
    }

    fn create_plane_image(
        &self,
        format: vk::Format,
        width: u32,
        height: u32,
    ) -> anyhow::Result<(vk::Image, vk::DeviceMemory)> {
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
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        unsafe {
            let image = self
                .device
                .create_image(&image_info, None)
                .map_err(|e| anyhow::anyhow!("vkCreateImage (plane): {e}"))?;

            let mem_reqs = self.device.get_image_memory_requirements(image);
            let memory_type_index = self
                .find_memory_type(
                    mem_reqs.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                )
                .ok_or_else(|| anyhow::anyhow!("no device-local memory type"))?;

            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(memory_type_index);

            let memory = self
                .device
                .allocate_memory(&alloc_info, None)
                .map_err(|e| anyhow::anyhow!("vkAllocateMemory (plane): {e}"))?;

            self.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| anyhow::anyhow!("vkBindImageMemory (plane): {e}"))?;

            Ok((image, memory))
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Vulkan API requires many parameters"
    )]
    fn copy_planes(
        &self,
        src: vk::Image,
        y_dst: vk::Image,
        uv_dst: vk::Image,
        y_width: u32,
        y_height: u32,
        uv_width: u32,
        uv_height: u32,
    ) -> anyhow::Result<()> {
        unsafe {
            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(self.command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd_bufs = self
                .device
                .allocate_command_buffers(&alloc_info)
                .map_err(|e| anyhow::anyhow!("allocate command buffer: {e}"))?;
            let cmd = cmd_bufs[0];

            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            self.device
                .begin_command_buffer(cmd, &begin_info)
                .map_err(|e| anyhow::anyhow!("begin command buffer: {e}"))?;

            // Transition src (NV12 DMA-BUF) to TRANSFER_SRC
            let src_barrier = vk::ImageMemoryBarrier::default()
                .image(src)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                .dst_queue_family_index(self.queue_family_index)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let y_barrier = vk::ImageMemoryBarrier::default()
                .image(y_dst)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });

            let uv_barrier = vk::ImageMemoryBarrier::default()
                .image(uv_dst)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
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
                &[src_barrier, y_barrier, uv_barrier],
            );

            // Copy Y plane (PLANE_0 → R8)
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
                src,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                y_dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[y_copy],
            );

            // Copy UV plane (PLANE_1 → RG8)
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
                src,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                uv_dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[uv_copy],
            );

            // Transition Y/UV to SHADER_READ_ONLY
            let y_to_read = vk::ImageMemoryBarrier::default()
                .image(y_dst)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let uv_to_read = vk::ImageMemoryBarrier::default()
                .image(uv_dst)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
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
                &[y_to_read, uv_to_read],
            );

            self.device
                .end_command_buffer(cmd)
                .map_err(|e| anyhow::anyhow!("end command buffer: {e}"))?;

            // Submit and wait
            let cmd_bufs = [cmd];
            let submit_info = vk::SubmitInfo::default().command_buffers(&cmd_bufs);
            let fence = self
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|e| anyhow::anyhow!("create fence: {e}"))?;

            self.device
                .queue_submit(self.queue, &[submit_info], fence)
                .map_err(|e| anyhow::anyhow!("queue submit: {e}"))?;

            self.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|e| anyhow::anyhow!("wait for fence: {e}"))?;

            self.device.destroy_fence(fence, None);
            self.device.free_command_buffers(self.command_pool, &[cmd]);

            Ok(())
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Vulkan API requires many parameters"
    )]
    fn wrap_as_wgpu_texture(
        &self,
        wgpu_device: &wgpu::Device,
        image: vk::Image,
        memory: vk::DeviceMemory,
        extra: Option<(vk::Image, vk::DeviceMemory)>,
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
            if let Some((extra_image, extra_memory)) = extra {
                device_clone.destroy_image(extra_image, None);
                device_clone.free_memory(extra_memory, None);
            }
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
