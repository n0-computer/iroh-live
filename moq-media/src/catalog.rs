use moq_mux::catalog::hang::CatalogExt;
use serde::{Deserialize, Serialize};

/// The iroh-live broadcast catalog: hang's base catalog (the `video` and
/// `audio` sections) extended with the iroh-live [`IrohLiveExt`] sections.
///
/// Use [`moq_mux::catalog::Producer`] to publish it and [`CatalogConsumer`] to
/// receive updates.
pub type Catalog = moq_mux::catalog::hang::Catalog<IrohLiveExt>;

/// Receives [`Catalog`] updates and lets a subscriber discover available tracks.
pub type CatalogConsumer = moq_mux::catalog::hang::Consumer<IrohLiveExt>;

/// The iroh-live catalog extension, flattened alongside hang's `video`/`audio`.
///
/// Carries the chat and user sections specific to iroh-live. Extending hang's
/// catalog through [`CatalogExt`] keeps it wire-compatible with base consumers,
/// which ignore the extra sections.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct IrohLiveExt {
    pub chat: Option<Chat>,
    pub user: Option<User>,
}

impl CatalogExt for IrohLiveExt {}

/// A reference to a track on the broadcast, as it appears in the catalog.
///
/// hang's own catalog entries carry a codec configuration alongside the name;
/// the iroh-live sections only need to say which track to subscribe to and how
/// urgently.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct TrackRef {
    /// The track name on the broadcast.
    pub name: String,
    /// The publisher's priority for the track, breaking ties between
    /// subscriptions of equal subscriber priority.
    pub priority: u8,
}

/// The chat section: which tracks carry messages and typing indicators.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Chat {
    /// The track carrying chat messages.
    pub message: Option<TrackRef>,
    /// The track carrying typing indicators, if the publisher sends any.
    pub typing: Option<TrackRef>,
}

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct User {
    pub id: Option<String>,
    pub name: Option<String>,
    pub avatar: Option<String>,
    pub color: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_flattens_into_catalog() {
        let mut catalog = Catalog::default();
        catalog.ext.chat = Some(Chat {
            message: Some(TrackRef {
                name: "chat".to_string(),
                priority: 10,
            }),
            typing: None,
        });
        catalog.ext.user = Some(User {
            name: Some("alice".to_string()),
            ..Default::default()
        });

        // chat and user flatten to the top level alongside video/audio.
        let json = serde_json::to_string(&catalog).expect("serialize");
        assert!(json.contains("\"chat\""), "chat section missing: {json}");
        assert!(json.contains("\"user\""), "user section missing: {json}");

        let parsed: Catalog = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.ext.chat, catalog.ext.chat);
        assert_eq!(parsed.ext.user, catalog.ext.user);
    }
}

#[cfg(test)]
mod interop_tests {
    use super::*;

    /// A catalog as `@moq/hang` publishes it from a browser: an `avc1` track,
    /// whose codec configuration is out of band, so the `description` carrying
    /// the avcC record is what makes it decodable at all.
    ///
    /// Our extension flattens into the same object, so a bug in how it is
    /// deserialized shows up as a dropped media field rather than as an error.
    const BROWSER_CATALOG: &str = r#"{
        "video": {
            "renditions": {
                "360p": {
                    "codec": "avc1.42e01e",
                    "description": "0142e01effe1001a6742e01e",
                    "codedWidth": 640,
                    "codedHeight": 360,
                    "bitrate": 1200000,
                    "framerate": 30
                }
            }
        },
        "audio": { "renditions": {} }
    }"#;

    #[test]
    fn browser_catalog_keeps_its_avcc_description() {
        let catalog: Catalog =
            serde_json::from_str(BROWSER_CATALOG).expect("a browser catalog should parse");
        let video = catalog
            .video
            .renditions
            .get("360p")
            .expect("the 360p rendition");
        assert!(
            video.description.is_some(),
            "an avc1 track without its description cannot be decoded",
        );
        assert_eq!(video.coded_width, Some(640));
    }
}
