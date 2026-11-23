//! The sidebar's destination switcher.
//!
//! It used to be a full-height rail beside the chat list. That rail was a
//! second left edge for the window: it took width from every conversation so
//! four icons could sit in a strip of their own. The destinations belong to
//! the sidebar, so the switcher now lives at the foot of it, as one floating
//! capsule that the list never has to compete with.

use gpui::prelude::*;
use gpui::{App, Entity, InteractiveElement, IntoElement, ParentElement, Styled, div};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, Selectable as _};

use crate::app::{Destination, WhatsAppApp};
use crate::components::parts;
use crate::responsive::ResponsiveLayout;
use crate::theme::Metrics;

/// The four destinations, in one capsule at the foot of the sidebar.
pub fn render_destination_pill(
    current: Destination,
    unread: [usize; 4],
    entity: Entity<WhatsAppApp>,
    layout: ResponsiveLayout,
    cx: &App,
) -> impl IntoElement + use<> {
    let metrics = *layout.metrics();
    let is_mobile = layout.is_mobile();
    // A phone's capsule is sized by the finger; a desktop one by the icon it
    // holds, so it does not become a second control scale in the window.
    let target = if is_mobile {
        metrics.touch_target()
    } else {
        metrics.icon_button()
    };
    let gap = if is_mobile {
        metrics.space_sm()
    } else {
        metrics.space_xxs()
    };

    let pill = div()
        .id("destination-pill")
        .flex()
        .items_center()
        .gap(gap)
        .p(metrics.space_xxs())
        .rounded_full()
        .bg(cx.theme().popover)
        .border_1()
        .border_color(cx.theme().border)
        .shadow_md()
        .children(
            Destination::ALL
                .into_iter()
                .zip(unread)
                .map(|(destination, badge)| {
                    render_destination(
                        destination,
                        destination == current,
                        badge,
                        target,
                        entity.clone(),
                        metrics,
                        cx,
                    )
                }),
        );

    if is_mobile {
        // The phone keeps the capsule across the foot, where a thumb reaches
        // it and where the old tab bar was.
        div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .justify_center()
            .pt(metrics.space_xs())
            .pb(metrics.space_sm())
            .bg(cx.theme().sidebar)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(pill)
    } else {
        // Floating over the list's lower edge. The caller positions and
        // occludes it; this wrapper only centres the capsule.
        div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .justify_center()
            .px(metrics.space_lg())
            .child(pill)
    }
}

/// One destination. It selects where the window is, the way a chat row selects
/// a conversation, and the selected one stays visibly selected.
fn render_destination(
    destination: Destination,
    is_current: bool,
    badge: usize,
    target: gpui::Pixels,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let icon: Icon = match destination {
        Destination::Chats => crate::components::ProductIcon::MessageSquare.into(),
        Destination::Status => crate::components::ProductIcon::CircleDashed.into(),
        Destination::Channels => crate::components::ProductIcon::Channels.into(),
        Destination::Communities => crate::components::ProductIcon::Users.into(),
    };

    let button = Button::new(destination.id())
        .ghost()
        .selected(is_current)
        .toggled(is_current)
        .accessibility_id(destination.id())
        .accessibility_label(destination.label())
        .debug_selector(move || destination.id().into())
        .relative()
        .size(target)
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .tooltip(destination.label())
        .when(is_current, |el| el.bg(cx.theme().list_active))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            log::info!("destination button pressed: {:?}", destination);
            entity.update(cx, |app, cx| app.set_destination(destination, window, cx));
        })
        .child(icon.size(metrics.icon()).text_color(if is_current {
            cx.theme().foreground
        } else {
            parts::subtle(cx)
        }));

    // Button clips its internal content to the icon box. Keep the badge beside
    // it so all digits survive on compact buttons too.
    div()
        .relative()
        .size(target)
        .flex_shrink_0()
        .child(button)
        .when(badge > 0, |el| {
            el.child(
                div()
                    .id(gpui::SharedString::from(format!(
                        "nav-badge-{}",
                        destination.id()
                    )))
                    .debug_selector(move || format!("nav-badge-{}", destination.id()))
                    .absolute()
                    .whitespace_nowrap()
                    .top(metrics.space_xs())
                    .right(metrics.space_xs())
                    .min_w(metrics.space_lg())
                    .min_h(metrics.space_lg())
                    .py(metrics.space_xxs())
                    .line_height(gpui::relative(1.0))
                    .px(metrics.space_xxs())
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(cx.theme().primary)
                    .font_features(crate::components::parts::tabular())
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().primary_foreground)
                    .child(div().debug_selector(|| "nav-badge-label".into()).child(
                        if badge > 99 {
                            "99+".to_string()
                        } else {
                            badge.to_string()
                        },
                    )),
            )
        })
}
