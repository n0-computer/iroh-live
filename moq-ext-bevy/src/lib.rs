//! Video from moq broadcasts on Bevy textures.
//!
//! [`VideoTexture`] is a component that holds a [`VideoFrames`] stream, from
//! a player or from a local source. [`VideoTexturePlugin`] draws the newest
//! frame of every such stream into an [`Image`] and sets it as the base color
//! texture of the entity's [`StandardMaterial`], so any mesh shows the video.
//!
//! Frames are drawn on the GPU, on Bevy's own device, by moq-video's
//! [`Renderer`]: a hardware-decoded DMA-BUF is imported without a copy, a CPU
//! frame is uploaded as its YUV planes, and a shader converts either to RGB.
//! A frame the renderer refuses, such as a GPU surface with no importer on this
//! platform, is converted to RGBA on the CPU and uploaded instead.
//!
//! [`Renderer`]: https://docs.rs/moq-video/latest/moq_video/render/struct.Renderer.html
//!
//! ```no_run
//! use bevy::prelude::*;
//! use moq_ext_bevy::{VideoTexture, VideoTexturePlugin};
//! # fn frames() -> moq_ext_sources::VideoFrames { unimplemented!() }
//!
//! fn setup(
//!     mut commands: Commands,
//!     mut meshes: ResMut<Assets<Mesh>>,
//!     mut materials: ResMut<Assets<StandardMaterial>>,
//! ) {
//!     commands.spawn((
//!         Mesh3d(meshes.add(Cuboid::default())),
//!         MeshMaterial3d(materials.add(StandardMaterial::default())),
//!         VideoTexture::new(frames()),
//!     ));
//! }
//!
//! App::new()
//!     .add_plugins((DefaultPlugins, VideoTexturePlugin::default()))
//!     .add_systems(Startup, setup)
//!     .run();
//! ```

use std::{collections::HashMap, sync::Arc};

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        render_asset::RenderAssets,
        render_resource::{Extent3d, TextureDimension, TextureFormat},
        renderer::{RenderDevice, RenderQueue},
        texture::GpuImage,
    },
};
pub use moq_ext_sources::VideoFrames;
use moq_video::{
    Frame, Surface,
    render::{Config, Renderer},
};
use tracing::{debug, info, warn};

/// Adds the systems that draw each [`VideoTexture`] into its material.
#[derive(Debug, Default, Clone, Copy)]
pub struct VideoTexturePlugin {
    /// Converts every frame to RGBA on the CPU instead of drawing it on the
    /// GPU, which downloads hardware-decoded frames. Meant for comparing the
    /// two paths and for working around a driver whose import misbehaves.
    pub cpu_only: bool,
}

impl Plugin for VideoTexturePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, take_video_frames);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            warn!("no render app, so video textures stay empty");
            return;
        };
        render_app
            .init_resource::<ExtractedVideos>()
            .insert_resource(VideoRenderers {
                cpu_only: self.cpu_only,
                videos: HashMap::new(),
            })
            .add_systems(ExtractSchedule, extract_videos)
            .add_systems(Render, draw_videos.in_set(RenderSystems::PrepareResources));
    }
}

/// Shows a video stream as the base color texture of the entity's material.
///
/// The entity needs a [`MeshMaterial3d<StandardMaterial>`]. Each frame,
/// [`VideoTexturePlugin`] takes the newest picture, if a new one arrived, and
/// draws it into the material's texture. The material should not be shared
/// with other entities, since they would show the same video.
#[derive(Component)]
pub struct VideoTexture {
    frames: VideoFrames,
    /// The frame taken this update, for the render world to draw.
    frame: Option<Arc<Frame>>,
    /// The image the frames are drawn into, once the first one arrived.
    image: Option<Handle<Image>>,
    /// The picture size the image was created at.
    size: UVec2,
}

impl std::fmt::Debug for VideoTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoTexture")
            .field("image", &self.image)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl VideoTexture {
    /// Creates a component that shows `frames`.
    pub fn new(frames: VideoFrames) -> Self {
        Self {
            frames,
            frame: None,
            image: None,
            size: UVec2::ZERO,
        }
    }

    /// Returns the image the video is drawn into, once the first frame arrived.
    pub fn image(&self) -> Option<&Handle<Image>> {
        self.image.as_ref()
    }

    /// Returns the size of the newest picture, or zero before the first.
    pub fn size(&self) -> UVec2 {
        self.size
    }
}

/// Takes the newest frame of every [`VideoTexture`] for the render world.
///
/// A frame of a new size gets a new, uninitialised image, set on the
/// material, since a texture cannot change size. The render world fills it.
fn take_video_frames(
    mut videos: Query<(&mut VideoTexture, &MeshMaterial3d<StandardMaterial>)>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (mut video, material) in &mut videos {
        video.frame = video.frames.try_next();
        let Some(frame) = &video.frame else {
            continue;
        };
        let size = UVec2::new(frame.surface.width(), frame.surface.height());
        if size == video.size && video.image.is_some() {
            continue;
        }
        debug!(width = size.x, height = size.y, "new video texture");
        let extent = Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        };
        let image = Image::new_uninit(
            extent,
            TextureDimension::D2,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::default(),
        );
        let handle = images.add(image);
        if let Some(mut material) = materials.get_mut(&material.0) {
            material.base_color_texture = Some(handle.clone());
        }
        video.image = Some(handle);
        video.size = size;
    }
}

/// One video as the render world sees it this frame.
struct ExtractedVideo {
    entity: Entity,
    image: AssetId<Image>,
    frame: Option<Arc<Frame>>,
}

/// The videos extracted this frame.
#[derive(Resource, Default)]
struct ExtractedVideos(Vec<ExtractedVideo>);

fn extract_videos(
    mut extracted: ResMut<ExtractedVideos>,
    videos: Extract<Query<(Entity, &VideoTexture)>>,
) {
    extracted.0.clear();
    for (entity, video) in &videos {
        if let Some(image) = &video.image {
            extracted.0.push(ExtractedVideo {
                entity,
                image: image.id(),
                frame: video.frame.clone(),
            });
        }
    }
}

/// How a frame reached its texture, logged when it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// A GPU surface imported without a copy, converted by a shader.
    GpuImport,
    /// CPU YUV planes uploaded, converted by a shader.
    GpuUpload,
    /// Converted to RGBA on the CPU and uploaded.
    Cpu,
}

/// The render-world state of one video.
struct VideoRenderer {
    /// `None` when the renderer could not be built on this device.
    renderer: Option<Renderer>,
    /// The newest frame not yet drawn, kept until its image is ready.
    pending: Option<Arc<Frame>>,
    path: Option<Path>,
}

/// One [`VideoRenderer`] per main-world video entity.
#[derive(Resource)]
struct VideoRenderers {
    cpu_only: bool,
    videos: HashMap<Entity, VideoRenderer>,
}

/// Draws each video's pending frame into its image's GPU texture.
fn draw_videos(
    extracted: Res<ExtractedVideos>,
    mut renderers: ResMut<VideoRenderers>,
    images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    let device = device.wgpu_device();
    let queue: &wgpu::Queue = &queue;
    let VideoRenderers { cpu_only, videos } = &mut *renderers;
    videos.retain(|entity, _| extracted.0.iter().any(|video| video.entity == *entity));
    for video in &extracted.0 {
        let state = videos.entry(video.entity).or_insert_with(|| {
            // Output is copied into the image, and `Rgba8Unorm` copies into
            // the image's sRGB format as is: the samples are gamma-encoded.
            let mut config = Config::new();
            config.usage = wgpu::TextureUsages::COPY_SRC;
            let renderer = (!*cpu_only)
                .then(|| Renderer::new(device, queue, config))
                .transpose()
                .inspect_err(
                    |err| warn!(error = %err, "no GPU video renderer, converting on the CPU"),
                )
                .ok()
                .flatten();
            VideoRenderer {
                renderer,
                pending: None,
                path: None,
            }
        });
        if let Some(frame) = &video.frame {
            state.pending = Some(frame.clone());
        }
        let Some(frame) = state.pending.clone() else {
            continue;
        };
        let Some(image) = images.get(video.image) else {
            continue;
        };
        if image.texture.width() != frame.surface.width()
            || image.texture.height() != frame.surface.height()
        {
            continue;
        }
        let path = draw(
            state.renderer.as_mut(),
            device,
            queue,
            &frame,
            &image.texture,
        );
        state.pending = None;
        if let Some(path) = path
            && state.path != Some(path)
        {
            info!(entity = %video.entity, ?path, decoded = surface_kind(&frame.surface), "video texture path");
            state.path = Some(path);
        }
    }
}

/// Draws `frame` into `target` and returns the path it took, or `None` when
/// it failed even on the CPU.
fn draw(
    renderer: Option<&mut Renderer>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &Frame,
    target: &wgpu::Texture,
) -> Option<Path> {
    let rendered = renderer.map(|renderer| renderer.render(frame));
    match rendered {
        Some(Ok(texture)) => {
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_texture(
                texture.as_image_copy(),
                target.as_image_copy(),
                texture.size(),
            );
            queue.submit([encoder.finish()]);
            return Some(match frame.surface {
                Surface::I420(_) => Path::GpuUpload,
                _ => Path::GpuImport,
            });
        }
        Some(Err(err)) => debug!(error = %err, "GPU video render failed, converting on the CPU"),
        None => {}
    }
    let rgba = frame
        .surface
        .to_rgba(&Default::default())
        .inspect_err(|err| warn!(error = %err, "could not convert a video frame to RGBA"))
        .ok()?;
    queue.write_texture(
        target.as_image_copy(),
        rgba.data(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(rgba.width() * 4),
            rows_per_image: None,
        },
        target.size(),
    );
    Some(Path::Cpu)
}

/// Names a surface's kind for the path log.
fn surface_kind(surface: &Surface) -> &'static str {
    match surface {
        Surface::I420(_) => "cpu i420",
        #[cfg(target_os = "linux")]
        Surface::DmaBuf(_) => "dma-buf",
        #[allow(
            unreachable_patterns,
            reason = "other surfaces exist only on some platforms"
        )]
        _ => "gpu surface",
    }
}
