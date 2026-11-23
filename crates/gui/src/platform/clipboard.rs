//! Reading media from the platform clipboard.
//!
//! One question — what did somebody copy — and one answer per platform. A
//! copied picture arrives as image bytes on every desktop; files copied in
//! the system's file manager arrive as paths on macOS and Windows, which is
//! the only way a video ever reaches the composer through the clipboard —
//! nobody copies video *bytes*. Linux reports neither file list (both its
//! backends declare `text/uri-list` without reading it), so file
//! copy-paste degrades there and the drag-and-drop path covers it.
//!
//! A page is its own story: pastes arrive through the document's `paste`
//! event (see [`crate::platform::drop`]'s listener), which needs no
//! permission prompt. The asynchronous `navigator.clipboard.read()` API this
//! used to rely on does, and gpui's own paste handler already consumed the
//! event by the time it answers — so this half answers nothing and the event
//! listener owns web pastes outright.

use crate::platform::picker::{Chosen, Picked};

/// Read clipboard media, if the clipboard holds any.
///
/// Images are converted synchronously; copied files are read off the UI
/// thread, the way the file chooser reads them. Text is not media and reads
/// back empty, which the caller treats as "nothing pasted" rather than a
/// failure.
pub fn read(cx: &gpui::App) -> gpui::Task<Result<Chosen, String>> {
    imp::read(cx)
}

/// Whether the clipboard currently holds a picture or copied files.
///
/// Asked synchronously so an action can decide whether to consume Ctrl+V or
/// let it paste text into a caption field. It is only a gate, not the read:
/// the actual bytes still arrive through [`read`], off the drawing thread.
pub fn holds_media(cx: &gpui::App) -> bool {
    cx.read_from_clipboard().is_some_and(|item| {
        item.entries.iter().any(|entry| {
            matches!(
                entry,
                gpui::ClipboardEntry::Image(_) | gpui::ClipboardEntry::ExternalPaths(_)
            )
        })
    })
}

/// Whether composer paste actions read bytes themselves. The page reads
/// files from its document event instead, so its action is a no-op and must
/// not claim the incoming-file slot ahead of that event listener.
pub fn reads_composer_paste() -> bool {
    imp::READS_COMPOSER_PASTE
}

/// An image entry as a file that can be sent.
///
/// Windows hands a copied bitmap over as `CF_DIB`, which gpui turns into a
/// BMP. Nothing downstream draws a BMP as a photo — the preview only accepts
/// JPEG, PNG, GIF and WebP, and a BMP is sent as a document — so a screenshot
/// pasted from the Snipping Tool or a browser arrived with no preview and as
/// the wrong kind. Convert it here, before it becomes a `Picked`, so every
/// caller sees the same picture.
///
/// The format the platform reports is a hint, not the answer: the same
/// clipboard can carry PNG bytes under a BMP label. The magic bytes decide
/// what the file really is, which is the rule the file chooser already uses.
fn image_as_picked(image: gpui::Image) -> Result<Picked, String> {
    let reported = image.format.mime_type().to_string();
    let sniffed = crate::platform::picker::image_mime_from_bytes(&image.bytes);
    let mime = sniffed.unwrap_or(&reported).to_string();

    if mime == "image/bmp" {
        let bytes = bmp_to_png(&image.bytes)
            .ok_or_else(|| "That copied picture could not be converted for sending.".to_string())?;
        let file_name = "pasted.png".to_string();
        if let Some(reason) = crate::platform::picker::unsendable(&file_name, bytes.len() as u64) {
            return Err(reason);
        }
        return Ok(Picked::automatic(file_name, "image/png".to_string(), bytes));
    }

    let extension = extension_for(&mime).unwrap_or_else(|| image.format.extension());
    let file_name = format!("pasted.{extension}");
    if let Some(reason) = crate::platform::picker::unsendable(&file_name, image.bytes.len() as u64)
    {
        return Err(reason);
    }
    Ok(Picked::automatic(file_name, mime, image.bytes))
}

/// The extension for a MIME type the clipboard can carry.
///
/// `Picked` names a file, and the name is how the send path and the preview
/// decide what they are looking at. Deriving it from the bytes keeps
/// `pasted.jpg` from carrying PNG data.
fn extension_for(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// A copied bitmap, as the PNG the rest of the tree can draw and send.
fn bmp_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    use image::ImageFormat;
    use std::io::Cursor;

    let decoded = image::load_from_memory_with_format(bytes, ImageFormat::Bmp).ok()?;
    let mut png = Cursor::new(Vec::new());
    decoded.write_to(&mut png, ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

/// Sort an item's entries into images and copied file paths.
///
/// Text entries are skipped, not refused: pasting text is the composer's own
/// job, and a clipboard holding both words and a picture pastes the words
/// through the field while the picture waits in the confirmation modal.
fn split_entries(item: gpui::ClipboardItem) -> (Vec<Picked>, Vec<std::path::PathBuf>, Vec<String>) {
    let mut images = Vec::new();
    let mut paths = Vec::new();
    let mut refused = Vec::new();
    for entry in item.into_entries() {
        match entry {
            gpui::ClipboardEntry::Image(image) => match image_as_picked(image) {
                Ok(picked) => images.push(picked),
                Err(reason) => refused.push(reason),
            },
            gpui::ClipboardEntry::ExternalPaths(external) => {
                paths.extend(external.paths().iter().cloned());
            }
            gpui::ClipboardEntry::String(_) => {}
        }
    }
    // A copied image often arrives twice: once as bytes and once as the
    // path of the file it came from (Raycast and Explorer both do this).
    // The image entry is the one that can be sent; keeping the path as well
    // queued the same picture twice and left its file name in the composer.
    if !images.is_empty() {
        paths.retain(|path| !is_image_path(path));
    }
    (images, paths, refused)
}

fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "heic"
            )
        })
}

/// Charge already-in-hand images against the selection budget the copied
/// files will draw from, refusing what the trip cannot carry.
///
/// Images arrive as bytes, so unlike chooser files there is no read to skip
/// — but the ceiling they share with copied files still holds, or a
/// near-limit image beside a near-limit file would ride out as twice it.
fn fit_images(
    images: Vec<Picked>,
    budget: &mut crate::platform::picker::Budget,
) -> (Vec<Picked>, Vec<String>) {
    let mut kept = Vec::with_capacity(images.len());
    let mut refused = Vec::new();
    for image in images {
        let size = image.bytes.len() as u64;
        match budget.refuse(&image.file_name, size) {
            Some(reason) => refused.push(reason),
            None => {
                budget.took(size);
                kept.push(image);
            }
        }
    }
    (kept, refused)
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use super::{Chosen, split_entries};

    pub(super) const READS_COMPOSER_PASTE: bool = true;

    pub(super) fn read(cx: &gpui::App) -> gpui::Task<Result<Chosen, String>> {
        let Some(item) = cx.read_from_clipboard() else {
            return gpui::Task::ready(Ok(Chosen::default()));
        };
        // Everything that can be slow happens off the thread that draws: a
        // copied bitmap is decoded and re-encoded as PNG, and copied files
        // are read from disk. The clipboard itself is read here because it
        // is a short, synchronous platform call; the pixels are not.
        cx.background_executor().spawn(async move {
            let (images, paths, mut refused) = split_entries(item);
            let mut budget = crate::platform::picker::Budget::default();
            let (images, over_budget) = super::fit_images(images, &mut budget);
            refused.extend(over_budget);
            let mut chosen = crate::platform::picker::read_paths_seeded(&paths, &mut budget);
            chosen.files.splice(0..0, images);
            chosen.refused.splice(0..0, refused);
            Ok(chosen)
        })
    }
}

#[cfg(target_family = "wasm")]
mod imp {
    use super::Chosen;

    pub(super) const READS_COMPOSER_PASTE: bool = false;

    pub(super) fn read(_cx: &gpui::App) -> gpui::Task<Result<Chosen, String>> {
        // Answered by the document `paste` listener instead: it reads the
        // event's files directly — every kind, including videos gpui's own
        // paste handler skips — with no permission prompt. The asynchronous
        // clipboard API would ask for one and answer with what the event
        // already gave, so asking is strictly worse.
        gpui::Task::ready(Ok(Chosen::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::{fit_images, split_entries};
    use crate::platform::picker::{Budget, Picked};

    #[test]
    fn text_clipboard_holds_no_media() {
        let (images, paths, refused) =
            split_entries(gpui::ClipboardItem::new_string("text".into()));
        assert!(images.is_empty());
        assert!(paths.is_empty());
        assert!(refused.is_empty());
    }

    #[test]
    fn image_clipboard_becomes_a_picked_file() {
        let image = gpui::Image {
            format: gpui::ImageFormat::Png,
            bytes: vec![1, 2, 3],
            id: 0,
        };
        let (images, paths, refused) = split_entries(gpui::ClipboardItem::new_image(&image));
        assert!(paths.is_empty());
        assert!(refused.is_empty());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].file_name, "pasted.png");
        assert_eq!(images[0].mime_type, "image/png");
        assert_eq!(images[0].kind, velo_core::OutgoingMedia::Image);
        assert_eq!(images[0].bytes, vec![1, 2, 3]);
    }

    /// A copied bitmap reaches gpui as a BMP, because that is what Windows'
    /// `CF_DIB` becomes. It has to leave here as a PNG: otherwise the preview
    /// refuses it and the send path calls it a document.
    #[test]
    fn a_copied_bitmap_becomes_a_png_photo() {
        use image::{ImageFormat, Rgba, RgbaImage};
        use std::io::Cursor;

        let source = RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255]));
        let mut bmp = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut bmp, ImageFormat::Bmp)
            .expect("a fixture bitmap");
        let image = gpui::Image {
            format: gpui::ImageFormat::Bmp,
            bytes: bmp.into_inner(),
            id: 0,
        };
        let (images, paths, refused) = split_entries(gpui::ClipboardItem::new_image(&image));
        assert!(paths.is_empty());
        assert!(refused.is_empty());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].file_name, "pasted.png");
        assert_eq!(images[0].mime_type, "image/png");
        assert_eq!(images[0].kind, velo_core::OutgoingMedia::Image);
        assert!(images[0].bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    /// The platform's label is not trusted over the bytes. A PNG copied from
    /// a browser can arrive under the BMP format the clipboard also offers.
    #[test]
    fn the_magic_bytes_decide_the_extension() {
        let png = b"\x89PNG\r\n\x1a\nnot really a png, but the header is what names it".to_vec();
        let image = gpui::Image {
            format: gpui::ImageFormat::Bmp,
            bytes: png,
            id: 0,
        };
        let picked = super::image_as_picked(image).expect("a picked picture");
        assert_eq!(picked.file_name, "pasted.png");
        assert_eq!(picked.mime_type, "image/png");
    }

    #[test]
    fn copied_file_paths_sort_into_paths() {
        let path = std::path::PathBuf::from("/tmp/clipe.mp4");
        let item = gpui::ClipboardItem {
            entries: vec![gpui::ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                vec![path.clone()].into_iter().collect(),
            ))],
        };
        let (images, paths, refused) = split_entries(item);
        assert!(images.is_empty());
        assert!(refused.is_empty());
        assert_eq!(paths, vec![path]);
    }

    /// Images already in hand still share the trip budget with the copied
    /// files beside them: a near-limit image must refuse room for a
    /// near-limit file, not ride out beside it as twice the ceiling.
    #[test]
    fn images_share_the_trip_budget_with_copied_files() {
        let ceiling = velo_ipc::MAX_STAGED_BYTES;
        let mut budget = Budget::default();
        budget.took(ceiling - 8);
        let image = Picked::automatic("pasted.png".into(), "image/png".into(), vec![0; 16]);
        let (kept, refused) = fit_images(vec![image], &mut budget);
        assert!(kept.is_empty());
        assert_eq!(refused.len(), 1);
    }

    /// ...while an image that fits is charged, so the files after it draw
    /// from what is left rather than from a fresh ceiling.
    #[test]
    fn a_fitting_image_is_charged_before_the_files() {
        let ceiling = velo_ipc::MAX_STAGED_BYTES;
        let mut budget = Budget::default();
        let image = Picked::automatic("pasted.png".into(), "image/png".into(), vec![0; 16]);
        let (kept, refused) = fit_images(vec![image], &mut budget);
        assert_eq!(kept.len(), 1);
        assert!(refused.is_empty());
        assert!(budget.refuse("clipe.mp4", ceiling).is_some());
        assert!(budget.refuse("clipe.mp4", ceiling - 16).is_none());
    }
}
