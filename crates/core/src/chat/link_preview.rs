//! A link's own card, as WhatsApp carries it beside the text.
//!
//! The protocol sends this inside `extended_text_message`: a URL, a title, a
//! description and a small JPEG. The store keeps the original protobuf, so
//! history does not need a second table for any of it; this type is the
//! projection a bubble draws.

use serde::{Deserialize, Serialize};

/// The card WhatsApp shows under a message that contains a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreview {
    /// The address the card opens.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    /// The small JPEG WhatsApp sends with the card. Empty when the sender's
    /// client could not make one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnail: Vec<u8>,
}

impl LinkPreview {
    /// The host to show when the card has no site name of its own.
    pub fn host(&self) -> &str {
        self.url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or(&self.url)
    }
}

#[cfg(test)]
mod tests {
    use super::LinkPreview;

    #[test]
    fn a_host_is_read_without_its_scheme_or_path() {
        let preview = LinkPreview {
            url: "https://example.com/a/b?q=1".into(),
            title: None,
            description: None,
            site_name: None,
            thumbnail: Vec::new(),
        };
        assert_eq!(preview.host(), "example.com");
    }
}
