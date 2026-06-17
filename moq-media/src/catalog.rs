use hang::catalog::{Audio, Video};
use serde::{Deserialize, Serialize};

#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Catalog {
    /// Video track information with multiple renditions.
    ///
    /// Contains a map of video track renditions that the viewer can choose from
    /// based on their preferences (resolution, bitrate, codec, etc).
    #[serde(default)]
    pub video: Video,

    /// Audio track information with multiple renditions.
    ///
    /// Contains a map of audio track renditions that the viewer can choose from
    /// based on their preferences (codec, bitrate, language, etc).
    #[serde(default)]
    pub audio: Audio,
    pub chat: Option<Chat>,
    pub user: Option<User>,
}

#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Chat {
    pub message: Option<moq_lite::Track>,
    pub typing: Option<moq_lite::Track>,
}

#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct User {
    pub id: Option<String>,
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub color: Option<String>,
}

/// A catalog consumer, used to receive catalog updates and discover tracks.
///
/// This wraps a `moq_lite::TrackConsumer` and automatically deserializes JSON
/// catalog data to discover available audio and video tracks in a broadcast.
#[derive(Clone, derive_more::Debug)]
#[debug("CatalogConsumer({})", track.name)]
pub struct CatalogConsumer {
    /// Access to the underlying track consumer.
    pub track: moq_lite::TrackConsumer,
    group: Option<moq_lite::GroupConsumer>,
}

impl CatalogConsumer {
    /// Create a new catalog consumer from a MoQ track consumer.
    pub fn new(track: moq_lite::TrackConsumer) -> Self {
        Self { track, group: None }
    }

    /// Get the next catalog update.
    ///
    /// This method waits for the next catalog publication and returns the
    /// catalog data. If there are no more updates, `None` is returned.
    pub async fn next(&mut self) -> anyhow::Result<Option<Catalog>> {
        loop {
            tokio::select! {
                res = self.track.next_group() => {
                    match res? {
                        Some(group) => {
                            // Use the new group.
                            self.group = Some(group);
                        }
                        // The track has ended, so we should return None.
                        None => return Ok(None),
                    }
                },
                Some(frame) = async { self.group.as_mut()?.read_frame().await.transpose() } => {
                    self.group.take(); // We don't support deltas yet
                    let catalog: Catalog = serde_json::from_slice(&frame?)?;
                    return Ok(Some(catalog));
                }
            }
        }
    }

    /// Wait until the catalog track is closed.
    pub async fn closed(&self) -> anyhow::Result<()> {
        Ok(self.track.closed().await?)
    }
}

impl From<moq_lite::TrackConsumer> for CatalogConsumer {
    fn from(inner: moq_lite::TrackConsumer) -> Self {
        Self::new(inner)
    }
}
