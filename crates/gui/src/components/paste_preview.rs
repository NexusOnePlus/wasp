//! Files waiting above the composer.
//!
//! Pasted from the clipboard or dropped onto the conversation: a compact tray
//! with what would go out, a caption box, and Send/Cancel. Nothing is
//! uploaded before Send is pressed.
//!
//! This used to be a full-screen modal. A modal makes the conversation
//! disappear at the exact moment the reader wants to check what they are
//! replying to, and it hid the composer behind a scrim for a caption that
//! belongs next to it. The state is the same; only the surface moved.

use std::sync::Arc;

use gpui::{
    App, Entity, FocusHandle, Image, ImageSource, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, StyledImage as _, div, img,
    prelude::FluentBuilder as _,
};
use gpui_component::ActiveTheme as _;
use gpui_component::Disableable as _;
use gpui_component::IconName;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Paste;
use velo_core::OutgoingMedia;

use crate::app::WhatsAppApp;
use crate::components::parts;
use crate::theme::Metrics;

/// One file waiting in the tray, with its preview decoded where drawable.
///
/// The selected media kind and the file's byte signature both gate previews:
/// a document stays a document, and mislabeled bytes are decoded according to
/// their actual format.
pub struct PreviewFile {
    pub file: crate::platform::picker::Picked,
    pub image: Option<Arc<Image>>,
}

impl PreviewFile {
    /// One file waiting in the tray, decoding its preview image only where
    /// asked: the tray draws the picture for a sole file and a badge for a
    /// batch, so decoding every image of a batch retains copies nothing
    /// draws.
    pub fn with_preview(file: crate::platform::picker::Picked, decode: bool) -> Self {
        let image = (decode
            && matches!(
                file.kind,
                OutgoingMedia::Image | OutgoingMedia::Sticker | OutgoingMedia::Gif
            ))
        .then(|| crate::platform::picker::previewable_image_mime(&file.bytes))
        .flatten()
        .and_then(gpui::ImageFormat::from_mime_type)
        .map(|format| Arc::new(Image::from_bytes(format, file.bytes.clone())));
        Self { file, image }
    }
}

/// What the tray draws, borrowed for the frame.
pub struct PastePreviewProps<'a> {
    pub files: &'a [PreviewFile],
    pub list_scroll: &'a gpui::ScrollHandle,
    pub app: Entity<WhatsAppApp>,
    pub can_send: bool,
    pub focus_handle: &'a FocusHandle,
    pub metrics: Metrics,
}

pub fn render_attachment_tray(props: PastePreviewProps<'_>, cx: &App) -> impl IntoElement + use<> {
    let PastePreviewProps {
        files,
        list_scroll: _list_scroll,
        app,
        can_send,
        focus_handle,
        metrics,
    } = props;

    let cancel_app = app.clone();
    let send_app = app.clone();
    let add_app = app.clone();
    let paste_app = app.clone();

    div()
        .id("paste-preview")
        .debug_selector(|| "paste-preview".into())
        .track_focus(focus_handle)
        // Ctrl-V while the caption owns the keyboard belongs to the tray:
        // another picture appends, and text still reaches the caption through
        // propagation when the clipboard holds no media.
        .capture_action::<Paste>(move |_, _, cx| {
            if crate::platform::clipboard::holds_media(cx) {
                paste_app.update(cx, |app, cx| app.paste_media(cx));
                cx.stop_propagation();
            } else {
                cx.propagate();
            }
        })
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(metrics.space_lg())
        .px(metrics.space_xl())
        .py(metrics.space_md())
        .bg(cx.theme().secondary)
        .border_t_1()
        .border_color(cx.theme().border)
        .child(render_thumbnails(files, app, metrics, cx))
        .child(
            Button::new("paste-preview-add")
                .icon(IconName::Plus)
                .ghost()
                .small()
                .tooltip("Add more files")
                .on_click(move |_, _, cx| {
                    add_app.update(cx, |app, cx| app.add_to_pending_files(cx));
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(metrics.space_md())
                .child(
                    div()
                        .id("paste-preview-cancel")
                        .debug_selector(|| "paste-preview-cancel".into())
                        .child(
                            Button::new("paste-preview-cancel-button")
                                .label(crate::tr!(Cancel))
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    cancel_app.update(cx, |app, cx| {
                                        app.cancel_paste_preview(cx);
                                    });
                                }),
                        ),
                )
                .child(
                    div()
                        .id("paste-preview-send")
                        .debug_selector(|| "paste-preview-send".into())
                        .child(
                            Button::new("paste-preview-send-button")
                                .label(crate::tr!(Send))
                                .primary()
                                .disabled(!can_send)
                                .when(can_send, |button| button.cursor_pointer())
                                .on_click(move |_event, _window, cx| {
                                    send_app.update(cx, |app, cx| app.confirm_paste_preview(cx));
                                }),
                        ),
                ),
        )
}

/// Every waiting picture, each removable and each openable full-size.
fn render_thumbnails(
    files: &[PreviewFile],
    app: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let size = metrics.touch_target() * 1.7;
    div()
        .id("paste-preview-thumbs")
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(metrics.space_sm())
        .children(files.iter().enumerate().map(|(index, file)| {
            let expand_app = app.clone();
            let remove_app = app.clone();
            let image = file.image.clone();
            let has_image = image.is_some();
            div()
                .relative()
                .w(size)
                .h(size)
                .flex_shrink_0()
                .child(
                    div()
                        .id(("attachment-thumb", index))
                        .debug_selector(move || {
                            if has_image {
                                "paste-preview-image".into()
                            } else {
                                "paste-preview-file".into()
                            }
                        })
                        .size_full()
                        .overflow_hidden()
                        .rounded(metrics.radius_md())
                        .bg(cx.theme().background)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            expand_app.update(cx, |app, cx| {
                                app.expand_pending_file(index, cx);
                            });
                        })
                        .map(|el| match image {
                            Some(image) => el.child(
                                img(ImageSource::Image(image))
                                    .id(("attachment-image", index))
                                    .size_full()
                                    .object_fit(ObjectFit::Contain),
                            ),
                            None => el.child(
                                div()
                                    .px(metrics.space_xs())
                                    .text_size(metrics.text_micro())
                                    .text_color(parts::subtle(cx))
                                    .truncate()
                                    .child(file.file.file_name.clone()),
                            ),
                        }),
                )
                .child(
                    div().absolute().top_0().right_0().child(
                        Button::new(("attachment-remove", index))
                            .icon(IconName::Close)
                            .danger()
                            .ghost()
                            .small()
                            .tooltip("Remove")
                            .on_click(move |_, _, cx| {
                                remove_app.update(cx, |app, cx| {
                                    app.remove_pending_file(index, cx);
                                });
                            }),
                    ),
                )
        }))
}

/// One waiting picture, as large as the window allows.
pub fn render_attachment_lightbox(
    image: Arc<Image>,
    app: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let close_app = app.clone();
    let close_button_app = app;
    div()
        .id("attachment-lightbox")
        .debug_selector(|| "attachment-lightbox".into())
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p(metrics.space_xxxl())
        .bg(parts::scrim(cx).opacity(0.94))
        .on_click(move |_, _, cx| {
            close_app.update(cx, |app, cx| {
                app.collapse_pending_file(cx);
            });
        })
        .child(
            div()
                .id("attachment-lightbox-image")
                .max_w_full()
                .max_h_full()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    img(ImageSource::Image(image))
                        .id("attachment-lightbox-content")
                        .max_w_full()
                        .max_h_full()
                        .object_fit(ObjectFit::Contain),
                ),
        )
        .child(
            div()
                .absolute()
                .top(metrics.space_lg())
                .right(metrics.space_lg())
                .child(
                    Button::new("attachment-lightbox-close")
                        .icon(IconName::Close)
                        .ghost()
                        .on_click(move |_, _, cx| {
                            close_button_app.update(cx, |app, cx| {
                                app.collapse_pending_file(cx);
                            });
                        }),
                ),
        )
}
