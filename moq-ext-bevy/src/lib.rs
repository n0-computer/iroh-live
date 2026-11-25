//! Video from moq broadcasts on Bevy textures.
//!
//! [`VideoTexture`] is a component that holds a [`VideoFrames`] stream, from
//! a player or from a local source. [`VideoTexturePlugin`] draws the newest
//! frame of every such stream into an [`Image`] and sets it as the base color
//! texture of the entity's [`StandardMaterial`], so any mesh shows the video.
//!
//! Frames are converted to RGBA on the CPU with [`Surface::to_rgba`], which
//! downloads GPU surfaces first. That suits a few small pictures; a zero-copy
//! path through Bevy's render world is future work.
//!
//! [`Surface::to_rgba`]: https://docs.rs/moq-video/latest/moq_video/enum.Surface.html#method.to_rgba
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
//!     .add_plugins((DefaultPlugins, VideoTexturePlugin))
//!     .add_systems(Startup, setup)
//!     .run();
//! ```

use bevy::{
    asset::RenderAssetUsages,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
pub use moq_ext_sources::VideoFrames;
use tracing::{debug, warn};

/// Adds the system that draws each [`VideoTexture`] into its material.
#[derive(Debug, Default, Clone, Copy)]
pub struct VideoTexturePlugin;

impl Plugin for VideoTexturePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, update_video_textures);
    }
}

/// Shows a video stream as the base color texture of the entity's material.
///
/// The entity needs a [`MeshMaterial3d<StandardMaterial>`]. Each frame,
/// [`VideoTexturePlugin`] takes the newest picture, if a new one arrived, and
/// writes it into the material's texture. The material should not be shared
/// with other entities, since they would show the same video.
#[derive(Component, Debug)]
pub struct VideoTexture {
    frames: VideoFrames,
    /// The image the frames are written into, once the first one arrived.
    image: Option<Handle<Image>>,
    /// The picture size the image was created at.
    size: UVec2,
}

impl VideoTexture {
    /// Creates a component that shows `frames`.
    pub fn new(frames: VideoFrames) -> Self {
        Self {
            frames,
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

/// Writes the newest frame of every [`VideoTexture`] into its material.
///
/// A same-size frame overwrites the image's pixels. A frame of a new size
/// gets a new image, set on the material, since a texture cannot change size.
fn update_video_textures(
    mut videos: Query<(&mut VideoTexture, &MeshMaterial3d<StandardMaterial>)>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (mut video, material) in &mut videos {
        let Some(frame) = video.frames.try_next() else {
            continue;
        };
        let rgba = match frame.surface.to_rgba(&Default::default()) {
            Ok(rgba) => rgba,
            Err(err) => {
                warn!(error = %err, "could not convert a video frame to RGBA");
                continue;
            }
        };
        let size = UVec2::new(rgba.width(), rgba.height());
        if size == video.size
            && let Some(handle) = &video.image
            && let Some(mut image) = images.get_mut(handle)
        {
            image.data = Some(rgba.into_data());
            continue;
        }
        debug!(width = size.x, height = size.y, "new video texture");
        let image = Image::new(
            Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            rgba.into_data(),
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
