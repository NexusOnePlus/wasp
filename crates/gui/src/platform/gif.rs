//! KLIPY's GIF search, as the composer sees it.
//!
//! The network call is the only part that needs a key. Everything that can be
//! tested without one — URL building, the response shape, which format is a
//! preview and which is the file WhatsApp plays — is a pure function here.

use gpui::http_client::{AsyncBody, Request};
use gpui::{App, Task};
use serde::Deserialize;
use std::sync::Arc;

/// One GIF, with the two formats the UI needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifResult {
    pub id: String,
    pub title: String,
    /// The smallest animated preview. WebP first: smaller than GIF and the
    /// sticker decoder already knows how to play its frames.
    pub preview_url: Option<String>,
    /// The MP4 that is sent. WhatsApp's `gif_playback` message is a video.
    pub video_url: Option<String>,
    pub width: u32,
    pub height: u32,
}

/// The key KLIPY issued for this installation.
///
/// An environment variable rather than a settings field for now: the picker
/// says what is missing, and a key pasted into a shell is enough to try it.
/// A Settings pane can replace this without changing the request path.
pub fn api_key() -> Option<String> {
    std::env::var("WASP_KLIPY_KEY")
        .ok()
        .or_else(|| std::env::var("KLIPY_API_KEY").ok())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

/// Search KLIPY. An absent key is a sentence for the panel, not a panic.
pub fn search(
    cx: &App,
    query: String,
    customer_id: String,
) -> Task<Result<Vec<GifResult>, String>> {
    let Some(key) = api_key() else {
        return Task::ready(Err(
            "Add a KLIPY API key in WASP_KLIPY_KEY to search GIFs.".to_string()
        ));
    };
    let url = search_url(&key, &query, &customer_id);
    let client = cx.http_client();
    cx.spawn(async move |_| {
        let request = Request::get(&url)
            .header("Accept", "application/json")
            .body(AsyncBody::empty())
            .map_err(|error| format!("could not build the KLIPY request: {error}"))?;
        let response = client
            .send(request)
            .await
            .map_err(|error| format!("KLIPY search failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("KLIPY answered {}", response.status()));
        }
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        use futures_lite::AsyncReadExt as _;
        body.read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("could not read KLIPY's answer: {error}"))?;
        parse(&bytes)
    })
}

/// Fetch one of KLIPY's static resources: a preview frame or the MP4.
pub fn fetch(cx: &App, url: String) -> Task<Result<Arc<Vec<u8>>, String>> {
    let client = cx.http_client();
    cx.spawn(async move |_| {
        let request = Request::get(&url)
            .body(AsyncBody::empty())
            .map_err(|error| format!("could not build the request: {error}"))?;
        let response = client
            .send(request)
            .await
            .map_err(|error| format!("could not fetch {url}: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("{url} answered {}", response.status()));
        }
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        use futures_lite::AsyncReadExt as _;
        body.read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("could not read {url}: {error}"))?;
        Ok(Arc::new(bytes))
    })
}

/// The search endpoint, with the formats this client can use and no others.
fn search_url(key: &str, query: &str, customer_id: &str) -> String {
    format!(
        "https://api.klipy.com/api/v1/{key}/gifs/search?q={}&page=1&per_page=24\
         &format_filter=gif,webp,jpg,mp4&content_filter=high&customer_id={}",
        percent_encode(query),
        percent_encode(customer_id),
    )
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[derive(Deserialize)]
struct Response {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    #[serde(default)]
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: serde_json::Value,
    #[serde(default)]
    title: String,
    file: File,
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    sm: Formats,
    #[serde(default)]
    md: Formats,
}

#[derive(Default, Deserialize)]
struct Formats {
    #[serde(default)]
    gif: Option<Media>,
    #[serde(default)]
    webp: Option<Media>,
    #[serde(default)]
    jpg: Option<Media>,
    #[serde(default)]
    mp4: Option<Media>,
}

#[derive(Clone, Deserialize)]
struct Media {
    url: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

fn parse(bytes: &[u8]) -> Result<Vec<GifResult>, String> {
    let response: Response = serde_json::from_slice(bytes)
        .map_err(|error| format!("KLIPY sent an unreadable answer: {error}"))?;
    Ok(response
        .data
        .data
        .into_iter()
        .filter_map(|item| {
            // `sm` is the grid's format; `md` is the fallback when the small
            // one is absent. The MP4 is what actually travels to WhatsApp.
            let preview = item
                .file
                .sm
                .webp
                .clone()
                .or_else(|| item.file.sm.gif.clone())
                .or_else(|| item.file.sm.jpg.clone())
                .or_else(|| item.file.md.webp.clone())
                .or_else(|| item.file.md.gif.clone());
            let video = item
                .file
                .sm
                .mp4
                .clone()
                .or_else(|| item.file.md.mp4.clone())?;
            let (width, height) = preview
                .as_ref()
                .map(|media| (media.width, media.height))
                .unwrap_or((video.width, video.height));
            Some(GifResult {
                id: match item.id {
                    serde_json::Value::String(id) => id,
                    other => other.to_string(),
                },
                title: item.title,
                preview_url: preview.map(|media| media.url),
                video_url: Some(video.url),
                width,
                height,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{parse, percent_encode, search_url};

    #[test]
    fn a_query_is_encoded_once() {
        assert_eq!(percent_encode("happy birthday"), "happy%20birthday");
        assert!(search_url("k", "a b", "user").contains("q=a%20b"));
    }

    #[test]
    fn the_smallest_animated_preview_and_the_mp4_are_chosen() {
        let json = br#"{
          "result": true,
          "data": { "data": [{
            "id": 42,
            "title": "Hello",
            "file": {
              "sm": {
                "gif": {"url": "https://example.invalid/small.gif", "width": 100, "height": 80},
                "webp": {"url": "https://example.invalid/small.webp", "width": 100, "height": 80},
                "mp4": {"url": "https://example.invalid/small.mp4", "width": 100, "height": 80}
              },
              "md": {}
            }
          }] }
        }"#;
        let results = parse(json).expect("a KLIPY answer");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].preview_url.as_deref(),
            Some("https://example.invalid/small.webp")
        );
        assert_eq!(
            results[0].video_url.as_deref(),
            Some("https://example.invalid/small.mp4")
        );
        assert_eq!(results[0].width, 100);
    }

    #[test]
    fn an_item_without_a_video_is_not_sendable() {
        let json = br#"{"data":{"data":[{"id":1,"title":"x","file":{"sm":{"gif":{"url":"a"}}}}]}}"#;
        assert!(parse(json).expect("a readable answer").is_empty());
    }
}
