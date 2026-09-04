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

/// Catalog reference to a chat track: the name to subscribe to, and the
/// priority to subscribe at.
///
/// moq-lite 0.1's `Track` filled this role, carrying `{ name, priority }` and
/// deriving serde. 0.2 splits the two apart — the name is an argument to
/// `create_track`, and `track::Info` holds the rest with no name and no serde —
/// so the serialized shape lives here now. The JSON is unchanged, which matters:
/// `IrohLiveExt` is flattened into the root catalog, and the browser client
/// validates this section against an object schema whether or not it subscribes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatTrack {
    pub name: String,
    pub priority: u8,
}

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Chat {
    pub message: Option<ChatTrack>,
    pub typing: Option<ChatTrack>,
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
            message: Some(ChatTrack {
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
