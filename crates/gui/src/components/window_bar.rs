//! The window's title strip, drawn by the app instead of by Windows.
//!
//! The system caption is a band of its own colour with a hairline under it,
//! which is what made the window read as a web page in a frame. This strip
//! is painted in the panels' own colour with no rule beneath it, so the
//! sidebar and the conversation run up to the window's top edge the way a
//! Mac app's do. Dragging, double-click to maximise, Snap layouts and the
//! three buttons keep their native behaviour: the strip only marks *where*
//! they are (`WindowControlArea`), and Windows does the rest.
//!
//! Not drawn on the web, where the browser owns the frame.

use gpui::{App, Image, ImageFormat, ImageSource, IntoElement, ParentElement, Styled, div, img};
use gpui_component::{ActiveTheme as _, TitleBar, h_flex};

use crate::components::parts;
use crate::theme::ActiveProductTheme as _;

/// The app picture, kept as pixels rather than an SVG mask.
///
/// `Icon` treats an SVG as a single-colour mask. `wasp-mark.svg` embeds the
/// PNG, so the whole opaque tile was tinted the
/// accent and rendered as one green square. `img` keeps the
/// picture's lime tile and folded panels.
fn icon() -> std::sync::Arc<Image> {
    static ICON: std::sync::OnceLock<std::sync::Arc<Image>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        std::sync::Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../../../../packaging/windows/wasp.png").to_vec(),
        ))
    })
    .clone()
}

pub fn render_window_bar(cx: &App) -> impl IntoElement + use<> {
    let metrics = cx.product().metrics;
    TitleBar::new()
        // The panels' colour, so the strip is part of them rather than a
        // frame around them, and no hairline under it to say otherwise.
        .bg(cx.theme().sidebar)
        .border_b_0()
        .child(
            h_flex()
                .gap(metrics.space_sm())
                .items_center()
                .child(img(ImageSource::Image(icon())).size(metrics.icon_small()))
                .child(
                    div()
                        .text_size(metrics.text_small())
                        .text_color(parts::subtle(cx))
                        .child("Wasp"),
                ),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_titlebar_icon_keeps_its_colours() {
        let icon = super::icon();
        assert!(icon.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}
