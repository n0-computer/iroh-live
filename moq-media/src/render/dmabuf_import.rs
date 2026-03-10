//! Zero-copy DMA-BUF → wgpu texture import via raw Vulkan.
//!
//! Imports NV12 DMA-BUFs from VAAPI into wgpu textures using:
//! - `VK_EXT_image_drm_format_modifier` for tiling layout
//! - `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` for fd import
//!
//! Each NV12 plane is imported as a separate VkImage (R8 for Y, RG8 for UV)
//! with the DMA-BUF's DRM format modifier, then wrapped as wgpu textures
//! for the existing NV12→RGBA shader.
//!
//! Falls back to CPU upload when the DMA-BUF modifier is not supported by
//! the Vulkan driver (e.g. Intel ANV doesn't support Y-tiled for R8/RG8).

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
    /// Modifiers supported by Vulkan for R8 format (Y plane import).
    supported_r8_modifiers: Vec<u64>,
    /// Modifiers supported by Vulkan for RG8 format (UV plane import).
    supported_rg8_modifiers: Vec<u64>,
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

            // Log device name for diagnosing multi-GPU mismatches.
            let props = instance.get_physical_device_properties(physical_device);
            let device_name = CStr::from_ptr(props.device_name.as_ptr()).to_string_lossy();

            // Query supported DRM modifiers for R8 and RG8 (per-plane import).
            let supported_r8_modifiers =
                query_format_modifiers(&instance, physical_device, vk::Format::R8_UNORM);
            let supported_rg8_modifiers =
                query_format_modifiers(&instance, physical_device, vk::Format::R8G8_UNORM);

            debug!(
                "DMA-BUF importer on {device_name}: R8 modifiers={:?}, RG8 modifiers={:?}",
                supported_r8_modifiers
                    .iter()
                    .map(|m| format!("0x{m:x}"))
                    .collect::<Vec<_>>(),
                supported_rg8_modifiers
                    .iter()
                    .map(|m| format!("0x{m:x}"))
                    .collect::<Vec<_>>(),
            );

            Some(Self {
                device: raw_device,
                physical_device,
                instance,
                supported_r8_modifiers,
                supported_rg8_modifiers,
            })
        }
    }

    /// Import a DMA-BUF NV12 frame as separate Y and UV wgpu textures.
    ///
    /// Uses per-plane import: each plane is imported as a separate single-plane
    /// VkImage (R8 for Y, RG8 for UV) with its own DMA-BUF memory import.
    /// This avoids the multi-planar NV12 format which has limited modifier support.
    pub fn import_nv12(
        &self,
        wgpu_device: &wgpu::Device,
        info: &DmaBufInfo,
    ) -> anyhow::Result<ImportedNv12Frame> {
        let w = info.display_width;
        let h = info.display_height;
        let uv_w = info.coded_width / 2;
        let uv_h = info.coded_height / 2;

        // Check modifier compatibility before attempting import.
        if !self.supported_r8_modifiers.contains(&info.modifier) {
            return Err(anyhow::anyhow!(
                "DMA-BUF modifier 0x{:x} not supported for R8 import (supported: {:?})",
                info.modifier,
                self.supported_r8_modifiers
                    .iter()
                    .map(|m| format!("0x{m:x}"))
                    .collect::<Vec<_>>(),
            ));
        }
        if !self.supported_rg8_modifiers.contains(&info.modifier) {
            return Err(anyhow::anyhow!(
                "DMA-BUF modifier 0x{:x} not supported for RG8 import",
                info.modifier,
            ));
        }

        if info.planes.len() < 2 {
            return Err(anyhow::anyhow!("NV12 DMA-BUF must have at least 2 planes"));
        }

        // Import Y plane as R8
        let (y_image, y_memory) = self.import_plane(
            info,
            0,
            vk::Format::R8_UNORM,
            info.coded_width,
            info.coded_height,
        )?;
        let y_texture = match self.wrap_as_wgpu_texture(
            wgpu_device,
            y_image,
            y_memory,
            None,
            w,
            h,
            wgpu::TextureFormat::R8Unorm,
        ) {
            Ok(t) => t,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(y_image, None);
                    self.device.free_memory(y_memory, None);
                }
                return Err(e);
            }
        };

        // Import UV plane as RG8
        let (uv_image, uv_memory) =
            self.import_plane(info, 1, vk::Format::R8G8_UNORM, uv_w, uv_h)?;
        let uv_texture = match self.wrap_as_wgpu_texture(
            wgpu_device,
            uv_image,
            uv_memory,
            None,
            uv_w,
            uv_h,
            wgpu::TextureFormat::Rg8Unorm,
        ) {
            Ok(t) => t,
            Err(e) => {
                unsafe {
                    self.device.destroy_image(uv_image, None);
                    self.device.free_memory(uv_memory, None);
                }
                return Err(e);
            }
        };

        Ok(ImportedNv12Frame {
            y_texture,
            uv_texture,
            width: w,
            height: h,
        })
    }

    /// Import a single plane from a DMA-BUF as a VkImage + VkDeviceMemory.
    fn import_plane(
        &self,
        info: &DmaBufInfo,
        plane_idx: usize,
        format: vk::Format,
        width: u32,
        height: u32,
    ) -> anyhow::Result<(vk::Image, vk::DeviceMemory)> {
        let plane = &info.planes[plane_idx];

        let plane_layout = vk::SubresourceLayout::default()
            .offset(plane.offset as u64)
            .row_pitch(plane.pitch as u64)
            .size(0);

        let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(info.modifier)
            .plane_layouts(std::slice::from_ref(&plane_layout));

        let mut ext_mem_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

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
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut modifier_info)
            .push_next(&mut ext_mem_info);

        unsafe {
            let image = self.device.create_image(&image_info, None).map_err(|e| {
                anyhow::anyhow!("vkCreateImage (plane {plane_idx} {format:?}): {e}")
            })?;

            let mem_reqs = self.device.get_image_memory_requirements(image);

            // Dup fd — Vulkan takes ownership.
            let fd = libc::dup(info.fd.as_raw_fd());
            if fd < 0 {
                self.device.destroy_image(image, None);
                return Err(anyhow::anyhow!(
                    "dup DMA-BUF fd for plane {plane_idx}: {}",
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
                    anyhow::anyhow!("no suitable memory type for plane {plane_idx}")
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
                    return Err(anyhow::anyhow!("vkAllocateMemory (plane {plane_idx}): {e}"));
                }
            };

            self.device
                .bind_image_memory(image, memory, 0)
                .map_err(|e| {
                    self.device.free_memory(memory, None);
                    self.device.destroy_image(image, None);
                    anyhow::anyhow!("vkBindImageMemory (plane {plane_idx}): {e}")
                })?;

            Ok((image, memory))
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
