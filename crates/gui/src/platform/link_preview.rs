//! Fetching the card WhatsApp draws under a link.
//!
//! The receiving half needs no network: WhatsApp sends the card in the
//! message. This is the sending half — read the page's Open Graph fields,
//! shrink its image to a JPEG the protocol accepts, and hand the result to
//! the daemon with the text.
//!
//! Fetching arbitrary URLs from a desktop app is an SSRF surface. The guard
//! here refuses non-HTTP schemes and literal loopback/private addresses;
//! names that resolve to a private address later are a known limitation
//! shared with the avatar fetcher, and the size and time limits are what keep
//! a hostile page from becoming a hang instead.

use std::net::IpAddr;
use std::sync::Arc;

use gpui::http_client::{AsyncBody, HttpClient, Request};
use gpui::{App, Task};
use velo_core::LinkPreview;

const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// oEmbed answers are a few hundred bytes; anything larger is not one.
const MAX_OEMBED_BYTES: usize = 64 * 1024;
/// The longest edge of the JPEG the card draws.
///
/// A card is as wide as a bubble — a few hundred points — and the image has
/// to survive a high-density display without going soft. 240 was the old
/// number and it was visibly blurry once the thumbnail was drawn wider than a
/// stamp; 720 costs a few dozen kilobytes more and is sharp at every size the
/// card takes.
const MAX_THUMBNAIL_EDGE: u32 = 720;

/// The first address in a message, if it has one.
///
/// Deliberately conservative: one URL, the way the client that generated the
/// preview treats it. A message full of links gets a card for the first.
pub fn first_url(text: &str) -> Option<String> {
    for marker in ["https://", "http://"] {
        let Some(start) = text.find(marker) else {
            continue;
        };
        let rest = &text[start..];
        let end = rest
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\''))
            .unwrap_or(rest.len());
        let url = rest[..end]
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}'])
            .to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

pub fn fetch(cx: &App, url: String) -> Task<Result<LinkPreview, String>> {
    if !is_public_http(&url) {
        return Task::ready(Err("that address cannot be previewed".to_string()));
    }
    let client = cx.http_client();
    cx.spawn(async move |_| {
        // YouTube's page is more than a megabyte of inline player data and its
        // `og:image` sat past the old 512 KiB cut. Its oEmbed endpoint names
        // the title in a few hundred bytes, and the thumbnail addresses are
        // derivable from the video id, so the page is not needed at all.
        if let Some(id) = youtube_id(&url) {
            return Ok(youtube_preview(&client, &url, id).await);
        }
        let html = match get(&client, &url, MAX_HTML_BYTES).await {
            Ok(html) => html,
            Err(error) => {
                // A page that says "not found" or "not for you" still has an
                // address worth showing. WhatsApp draws a card for it; the
                // alternative was a link with no card at all, and a GitHub
                // private repository always answers 404 to an anonymous read.
                if answered_status(&error)
                    .is_some_and(|status| matches!(status, 401 | 403 | 404 | 410))
                {
                    log::info!("link preview: {url} has no public metadata");
                    return Ok(fallback_preview(&url));
                }
                return Err(error);
            }
        };
        let page = String::from_utf8_lossy(&html);
        let mut preview = parse_page(&page, &url);
        if let Some(image) = preview.take_image_url()
            && is_public_http(&image)
            && let Some(bytes) = fetch_image(&client, &image).await
        {
            preview.0.thumbnail = jpeg_thumbnail(&bytes).unwrap_or_default();
        }
        Ok(preview.0)
    })
}

/// The video id in a YouTube address, or `None` for anything else.
///
/// Only the shapes that name one video: a channel or a playlist has no
/// thumbnail to derive.
fn youtube_id(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host
        .split(':')
        .next()
        .unwrap_or(host)
        .trim_start_matches("www.")
        .trim_start_matches("m.")
        .trim_start_matches("music.");
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let id = if host == "youtu.be" {
        path.split('/').next().unwrap_or_default()
    } else if matches!(host, "youtube.com" | "youtube-nocookie.com") {
        let mut segments = path.split('/');
        match segments.next().unwrap_or_default() {
            "shorts" | "live" | "embed" | "v" => segments.next().unwrap_or_default(),
            "watch" => query_value(rest, "v").unwrap_or_default(),
            _ => return None,
        }
    } else {
        return None;
    };
    let valid = (6..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    valid.then_some(id)
}

fn query_value<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// A YouTube card without reading the player page.
async fn youtube_preview(client: &Arc<dyn HttpClient>, url: &str, id: &str) -> LinkPreview {
    #[derive(serde::Deserialize)]
    struct OEmbed {
        title: Option<String>,
        author_name: Option<String>,
        thumbnail_url: Option<String>,
    }
    let mut title = None;
    let mut description = None;
    let mut oembed_image = None;
    let oembed = format!(
        "https://www.youtube.com/oembed?format=json&url={}",
        percent_encode(url)
    );
    if let Ok(bytes) = get(client, &oembed, MAX_OEMBED_BYTES).await
        && let Ok(answer) = serde_json::from_slice::<OEmbed>(&bytes)
    {
        title = answer.title.filter(|value| !value.trim().is_empty());
        description = answer.author_name.filter(|value| !value.trim().is_empty());
        oembed_image = answer.thumbnail_url;
    }

    let mut candidates = vec![
        format!("https://i.ytimg.com/vi/{id}/maxresdefault.jpg"),
        format!("https://i.ytimg.com/vi/{id}/hq720.jpg"),
        format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"),
    ];
    if let Some(image) = oembed_image
        && !candidates.contains(&image)
    {
        candidates.push(image);
    }
    let mut thumbnail = Vec::new();
    for candidate in candidates {
        if let Some(bytes) = fetch_image(client, &candidate).await
            && let Some(jpeg) = jpeg_thumbnail(&bytes)
        {
            thumbnail = jpeg;
            break;
        }
    }
    LinkPreview {
        url: url.to_string(),
        title,
        description,
        site_name: Some("YouTube".to_string()),
        thumbnail,
    }
}

/// Try an image and the well-known smaller copies when it is missing.
async fn fetch_image(client: &Arc<dyn HttpClient>, url: &str) -> Option<Vec<u8>> {
    for candidate in image_candidates(url) {
        if let Ok(bytes) = get(client, &candidate, MAX_IMAGE_BYTES).await
            && !bytes.is_empty()
        {
            return Some(bytes);
        }
    }
    None
}

fn image_candidates(url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let upgraded = larger_image(url);
    if upgraded != url {
        out.push(upgraded);
    }
    out.push(url.to_string());
    // A `maxresdefault` that was never generated is common on older uploads;
    // every video has at least the 480-pixel `hqdefault`.
    if url.contains("maxresdefault") {
        for smaller in ["sddefault", "hqdefault"] {
            out.push(url.replace("maxresdefault", smaller));
        }
    }
    out
}

/// A card for an address whose page could not be read: the host, and the last
/// path segment as a title. No image, which the interface draws as a monogram.
fn fallback_preview(url: &str) -> LinkPreview {
    let host = normalized_host(url);
    let title = url
        .split(['?', '#'])
        .next()
        .and_then(|address| address.trim_end_matches('/').rsplit('/').next())
        .filter(|segment| !segment.is_empty() && !segment.contains('.'))
        .map(|segment| segment.replace(['-', '_'], " "))
        .or_else(|| (!host.is_empty()).then(|| host.clone()));
    LinkPreview {
        url: url.to_string(),
        title,
        description: None,
        site_name: (!host.is_empty()).then_some(host),
        thumbnail: Vec::new(),
    }
}

fn normalized_host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_ascii_lowercase()
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

/// The status behind an error `get` wrote, if it was an HTTP answer.
fn answered_status(error: &str) -> Option<u16> {
    let after = error.split(" answered ").nth(1)?;
    after
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// A higher-resolution address for an image that has a well-known larger
/// sibling, or the address unchanged.
///
/// Only where the name is a documented convention: guessing at arbitrary
/// suffixes turns a working card into a missing one.
fn larger_image(url: &str) -> String {
    for (small, large) in [
        ("hqdefault", "maxresdefault"),
        ("sddefault", "maxresdefault"),
        ("mqdefault", "maxresdefault"),
    ] {
        if url.contains(small) {
            return url.replace(small, large);
        }
    }
    url.to_string()
}

/// A parsed page, with the image address held separately until it is fetched.
struct Parsed(LinkPreview, Option<String>);

impl Parsed {
    fn take_image_url(&mut self) -> Option<String> {
        self.1.take()
    }
}

fn parse_page(html: &str, url: &str) -> Parsed {
    // Every tag worth reading lives in the head, and the head is where a big
    // page puts its scripts too. Stopping there keeps the scan small without
    // guessing a byte count: YouTube's metadata begins around 846 KiB, well
    // past the old 512 KiB cut.
    let end = find_ci(html, "</head>", 0)
        .map(|at| at + "</head>".len())
        .unwrap_or(html.len());
    let html = &html[..end];
    let mut title = None;
    let mut description = None;
    let mut site_name = None;
    let mut image = None;
    let mut at = 0;
    while let Some(start) = find_ci(html, "<meta", at) {
        let Some(close) = html[start..].find('>').map(|close| start + close + 1) else {
            break;
        };
        let tag = &html[start..close];
        let key = attr(tag, "property")
            .or_else(|| attr(tag, "name"))
            .map(|key| key.to_ascii_lowercase());
        let content = attr(tag, "content");
        if let (Some(key), Some(content)) = (key, content) {
            match key.as_str() {
                "og:title" | "twitter:title" if title.is_none() => {
                    title = Some(decode_entities(&content))
                }
                "og:description" | "description" | "twitter:description"
                    if description.is_none() =>
                {
                    description = Some(decode_entities(&content))
                }
                "og:site_name" if site_name.is_none() => {
                    site_name = Some(decode_entities(&content))
                }
                "og:image" | "og:image:url" | "twitter:image" if image.is_none() => {
                    image = Some(absolute(url, &decode_entities(&content)))
                }
                _ => {}
            }
        }
        at = close;
    }
    if title.is_none()
        && let Some(start) = find_ci(html, "<title>", 0)
        && let Some(end) = find_ci(html, "</title>", start + 7)
    {
        let value = decode_entities(html[start + 7..end].trim());
        if !value.is_empty() {
            title = Some(value);
        }
    }
    Parsed(
        LinkPreview {
            url: url.to_string(),
            title,
            description,
            site_name,
            thumbnail: Vec::new(),
        },
        image,
    )
}

/// Find an ASCII marker without lowercasing (and reallocating) the page.
///
/// Lowercasing the whole document changed the byte length of some non-ASCII
/// characters, so offsets found in the lowered copy no longer sliced the
/// original correctly. Every marker here is ASCII; comparing bytes ignores
/// case and cannot land inside a multi-byte character.
fn find_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let needle = needle.as_bytes();
    if from >= bytes.len() || needle.is_empty() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|at| from + at)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let at = find_ci(tag, &needle, 0)?;
    let rest = &tag[at + needle.len()..];
    let quote = rest.chars().next()?;
    let value = if quote == '"' || quote == '\'' {
        rest[1..].split(quote).next()?
    } else {
        rest.split_whitespace().next()?
    };
    Some(value.to_string())
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

fn absolute(base: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    if let Some(rest) = url.strip_prefix("//") {
        return format!("https://{rest}");
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    let host = rest.split('/').next().unwrap_or(rest);
    let origin = format!("{scheme}://{host}");
    if url.starts_with('/') {
        return format!("{origin}{url}");
    }
    let Some((prefix, _)) = base.rsplit_once('/') else {
        return url.to_string();
    };
    format!("{prefix}/{url}")
}

fn is_public_http(url: &str) -> bool {
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host == "0.0.0.0"
        || host.ends_with(".local")
        || host.ends_with(".localhost")
    {
        return false;
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        return match address {
            IpAddr::V4(address) => {
                !(address.is_loopback()
                    || address.is_private()
                    || address.is_link_local()
                    || address.is_unspecified()
                    || address.is_multicast())
            }
            IpAddr::V6(address) => {
                let first = address.segments()[0];
                !(address.is_loopback()
                    || address.is_unspecified()
                    || address.is_multicast()
                    || (first & 0xfe00) == 0xfc00
                    || (first & 0xffc0) == 0xfe80)
            }
        };
    }
    true
}

async fn get(client: &Arc<dyn HttpClient>, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let request = Request::get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Wasp/1.2",
        )
        .header("Accept", "text/html,image/*,*/*;q=0.8")
        .header("Accept-Language", "en-US,en;q=0.9")
        // Plain bytes: the page parsers read strings, and an encoded body
        // that the HTTP client does not decode is a page with no metadata.
        .header("Accept-Encoding", "identity")
        .body(AsyncBody::empty())
        .map_err(|error| format!("could not build the request: {error}"))?;
    let response = client
        .send(request)
        .await
        .map_err(|error| format!("could not fetch {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let body = response.into_body();
    let mut bytes = Vec::new();
    use futures_lite::AsyncReadExt as _;
    body.take(limit as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("could not read {url}: {error}"))?;
    Ok(bytes)
}

fn jpeg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    use image::ImageReader;
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    use std::io::Cursor;

    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    // Lanczos rather than the nearest-neighbour `thumbnail`: the card draws
    // this at several hundred points, and the cheap filter was what made a
    // sharp page image look like a screenshot of a screenshot.
    let small = image.resize(MAX_THUMBNAIL_EDGE, MAX_THUMBNAIL_EDGE, FilterType::Lanczos3);
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut Cursor::new(&mut out), 82)
        .encode_image(&small)
        .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{absolute, first_url, is_public_http, larger_image, parse_page, youtube_id};

    #[test]
    fn the_first_http_address_is_the_one_previewed() {
        let text = "look (https://example.com/a) and http://second.test";
        assert_eq!(first_url(text).as_deref(), Some("https://example.com/a"));
        assert!(first_url("no link here").is_none());
    }

    #[test]
    fn private_and_non_http_addresses_are_refused() {
        assert!(is_public_http("https://example.com/a"));
        assert!(!is_public_http("http://127.0.0.1/"));
        assert!(!is_public_http("http://192.168.1.2/"));
        assert!(!is_public_http("file:///etc/passwd"));
    }

    #[test]
    fn a_known_image_convention_upgrades_to_the_larger_copy() {
        assert_eq!(
            larger_image("https://i.ytimg.com/vi/abc/hqdefault.jpg"),
            "https://i.ytimg.com/vi/abc/maxresdefault.jpg"
        );
        assert_eq!(
            larger_image("https://example.com/card.png"),
            "https://example.com/card.png"
        );
    }

    #[test]
    fn open_graph_fields_are_read_and_relative_images_resolve() {
        let page = r#"<html><head>
            <meta property="og:title" content="Example &amp; Co">
            <meta property="og:description" content="A page">
            <meta property="og:site_name" content="Example">
            <meta property="og:image" content="/card.png">
            </head></html>"#;
        let parsed = parse_page(page, "https://example.com/a");
        assert_eq!(parsed.0.title.as_deref(), Some("Example & Co"));
        assert_eq!(parsed.0.description.as_deref(), Some("A page"));
        assert_eq!(parsed.0.site_name.as_deref(), Some("Example"));
        assert_eq!(parsed.1.as_deref(), Some("https://example.com/card.png"));
        assert_eq!(
            absolute("https://example.com/a", "/x.png"),
            "https://example.com/x.png"
        );
    }

    #[test]
    fn metadata_past_half_a_megabyte_is_still_found() {
        // YouTube puts its `og:image` around 846 KiB into the document. The
        // old 512 KiB cut never reached it, so every YouTube card had no
        // picture.
        let mut page = String::from("<html><head>");
        page.push_str(&"<script>var filler = 1;</script>".repeat(30_000));
        assert!(page.len() > 512 * 1024);
        page.push_str(
            r#"<meta property="og:title" content="A big page">
               <meta property="og:image" content="https://example.com/big.jpg">
               </head><body>body</body></html>"#,
        );
        let parsed = parse_page(&page, "https://example.com/big");
        assert_eq!(parsed.0.title.as_deref(), Some("A big page"));
        assert_eq!(parsed.1.as_deref(), Some("https://example.com/big.jpg"));
    }

    #[test]
    fn tags_are_found_without_changing_the_byte_offsets() {
        // A character whose lowercase form is longer than the original used
        // to shift every offset after it.
        let page = "<html><head><title>İstanbul</title><meta property=\"og:image\" content=\"/x.png\"></head>";
        let parsed = parse_page(page, "https://example.com/a");
        assert_eq!(parsed.0.title.as_deref(), Some("İstanbul"));
        assert_eq!(parsed.1.as_deref(), Some("https://example.com/x.png"));
    }

    #[test]
    fn every_youtube_shape_names_its_video() {
        for (url, id) in [
            ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            (
                "https://youtube.com/shorts/6teFLQvApHE?si=abc",
                "6teFLQvApHE",
            ),
            ("https://youtu.be/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
            (
                "https://m.youtube.com/watch?v=32zCRIQTjD4&t=4",
                "32zCRIQTjD4",
            ),
            ("https://www.youtube.com/live/ABCDEF12345", "ABCDEF12345"),
            ("https://www.youtube.com/embed/ABCDEF12345", "ABCDEF12345"),
        ] {
            assert_eq!(youtube_id(url), Some(id), "{url}");
        }
        assert_eq!(youtube_id("https://www.youtube.com/@RickAstleyYT"), None);
        assert_eq!(youtube_id("https://example.com/watch?v=abc"), None);
    }

    #[test]
    fn a_missing_maxres_falls_back_to_the_smaller_copies() {
        assert_eq!(
            super::image_candidates("https://i.ytimg.com/vi/abc/maxresdefault.jpg"),
            vec![
                "https://i.ytimg.com/vi/abc/maxresdefault.jpg",
                "https://i.ytimg.com/vi/abc/sddefault.jpg",
                "https://i.ytimg.com/vi/abc/hqdefault.jpg",
            ]
        );
    }

    #[test]
    fn an_unreadable_page_still_gets_a_card() {
        let preview = super::fallback_preview("https://github.com/example/table-tennis");
        assert_eq!(preview.title.as_deref(), Some("table tennis"));
        assert_eq!(preview.site_name.as_deref(), Some("github.com"));
        assert!(preview.thumbnail.is_empty());
        assert_eq!(
            super::answered_status("https://x answered 404 Not Found"),
            Some(404)
        );
    }
}
