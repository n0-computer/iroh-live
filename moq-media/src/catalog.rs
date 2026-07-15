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

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Chat {
    pub message: Option<moq_lite::Track>,
    pub typing: Option<moq_lite::Track>,
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
            message: Some(moq_lite::Track {
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
