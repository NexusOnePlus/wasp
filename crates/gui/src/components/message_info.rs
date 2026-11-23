//! Who has seen one of our messages.
//!
//! WhatsApp's message info is a list of the people the message reached, with
//! the strongest receipt each one sent. The aggregate tick says whether
//! anybody read it; this says who.

use gpui::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div,
};
use gpui_component::ActiveTheme as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use velo_core::MessageStatus;

use crate::app::WhatsAppApp;
use crate::components::parts;
use crate::theme::Metrics;

pub fn render_message_info(
    rows: &[(String, MessageStatus)],
    preview: &str,
    app: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let close_app = app.clone();
    div()
        .id("message-info")
        .debug_selector(|| "message-info".into())
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(parts::scrim(cx).opacity(0.55))
        .on_click(move |_, _, cx| {
            close_app.update(cx, |app, cx| app.close_message_info(cx));
        })
        .child(
            div()
                .id("message-info-panel")
                .w_full()
                .max_w(metrics.space_xxxl() * 10.0)
                .max_h(metrics.space_xxxl() * 10.0)
                .overflow_hidden()
                .rounded(metrics.radius_xl())
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .shadow_lg()
                .flex()
                .flex_col()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px(metrics.space_lg())
                        .py(metrics.space_md())
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_size(metrics.text_title())
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Message info"),
                        )
                        .child({
                            let close_app = app.clone();
                            Button::new("message-info-close")
                                .icon(gpui_component::IconName::Close)
                                .ghost()
                                .small()
                                .on_click(move |_, _, cx| {
                                    close_app.update(cx, |app, cx| {
                                        app.close_message_info(cx);
                                    });
                                })
                        }),
                )
                .children((!preview.is_empty()).then(|| {
                    div()
                        .px(metrics.space_lg())
                        .py(metrics.space_md())
                        .text_size(metrics.text_small())
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(preview.to_string())
                }))
                .child(
                    div()
                        .id("message-info-rows")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .py(metrics.space_xs())
                        .children(rows.is_empty().then(|| {
                            div()
                                .px(metrics.space_lg())
                                .py(metrics.space_md())
                                .text_size(metrics.text_small())
                                .text_color(cx.theme().muted_foreground)
                                .child("No receipts yet.")
                        }))
                        .children(rows.iter().map(|(name, status)| {
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .px(metrics.space_lg())
                                .py(metrics.space_sm())
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(metrics.text_small())
                                        .text_color(cx.theme().foreground)
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(metrics.text_meta())
                                        .text_color(match status {
                                            MessageStatus::Read | MessageStatus::Played => {
                                                cx.theme().primary
                                            }
                                            _ => cx.theme().muted_foreground,
                                        })
                                        .child(status_label(*status)),
                                )
                        })),
                ),
        )
}

fn status_label(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::Delivered => "Delivered",
        MessageStatus::Read => "Read",
        MessageStatus::Played => "Played",
        MessageStatus::Sent => "Sent",
        MessageStatus::Pending => "Pending",
        MessageStatus::Failed => "Failed",
    }
}
