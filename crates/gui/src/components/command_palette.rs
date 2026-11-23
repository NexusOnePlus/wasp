//! The floating command bar.
//!
//! One field, one list, three sections. It floats over the conversation
//! instead of replacing it, because the question it answers — "where is that
//! chat?" — is asked while looking at something else. The sidebar's field is
//! still the filter for the list on screen; this is the way to leave it.

use gpui::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
};
use gpui_component::ActiveTheme as _;
use gpui_component::input::Input;
use gpui_component::{Icon, IconName};

use crate::app::WhatsAppApp;
use crate::app::palette::{Palette, PaletteKind, PaletteRow};
use crate::components::{Avatar, ProductIcon, parts};
use crate::theme::Metrics;

pub fn render_command_palette(
    palette: &Palette,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let panel = div()
        .id("command-palette-panel")
        .w_full()
        .max_w(metrics.space_xxxl() * 12.0)
        .rounded(metrics.radius_xl())
        .bg(cx.theme().popover)
        .border_1()
        .border_color(cx.theme().border)
        .shadow_lg()
        .overflow_hidden()
        .flex()
        .flex_col()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_center()
                .gap(metrics.space_md())
                .px(metrics.space_lg())
                .py(metrics.space_md())
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Icon::new(IconName::Search)
                        .size(metrics.icon_small())
                        .text_color(parts::subtle(cx)),
                )
                .children(palette.input.as_ref().map(|input| {
                    div()
                        .id("command-palette-input")
                        .debug_selector(|| "command-palette-input".into())
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(input).w_full())
                })),
        )
        .child(
            div()
                .id("command-palette-results")
                .max_h(metrics.space_xxxl() * 8.0)
                .overflow_y_scroll()
                .py(metrics.space_xs())
                .children(render_rows(palette, entity.clone(), metrics, cx)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(metrics.space_lg())
                .py(metrics.space_sm())
                .border_t_1()
                .border_color(cx.theme().border)
                .text_size(metrics.text_micro())
                .text_color(parts::subtle(cx))
                .child("↑ ↓ move · Enter open · Esc close")
                .child(if palette.messages_loading {
                    "Searching messages…"
                } else {
                    "Search chats, messages and actions"
                }),
        );

    div()
        .id("command-palette")
        .debug_selector(|| "command-palette".into())
        // This is what the `up` and `down` bindings in `init_app_bindings`
        // are scoped to. Without it the surface was drawn but the keymap had
        // no context to match, so the footer promised movement the arrows
        // never delivered.
        .key_context("CommandPalette")
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(metrics.space_xxxl() * 2.0)
        .px(metrics.space_xl())
        .bg(parts::scrim(cx).opacity(0.28))
        .on_click({
            let entity = entity.clone();
            move |_, _, cx| {
                entity.update(cx, |app, cx| {
                    app.close_command_palette(cx);
                });
            }
        })
        .child(panel)
}

fn render_rows<'a>(
    palette: &'a Palette,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &'a App,
) -> impl Iterator<Item = gpui::AnyElement> + 'a {
    let mut previous: Option<PaletteKind> = None;
    palette.rows.iter().enumerate().map(move |(index, row)| {
        let heading = (previous != Some(row.kind)).then(|| {
            previous = Some(row.kind);
            div()
                .px(metrics.space_lg())
                .pt(metrics.space_sm())
                .pb(metrics.space_xxs())
                .text_size(metrics.text_micro())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(parts::subtle(cx))
                .child(row.kind.heading())
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .children(heading)
            .child(render_row(
                row,
                index,
                index == palette.selected,
                entity.clone(),
                metrics,
                cx,
            ))
            .into_any_element()
    })
}

fn render_row(
    row: &PaletteRow,
    index: usize,
    selected: bool,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let avatar: AnyElement = match row.jid.as_ref() {
        Some(jid) => Avatar::new(
            SharedString::from(jid.clone()),
            &row.title,
            metrics.avatar_inline(),
        )
        .picture(row.avatar_key.clone(), None)
        .on(cx.theme().popover)
        .into_any_element(),
        None => div().into_any_element(),
    };
    let action_icon: Option<AnyElement> = match row.action {
        Some(crate::app::palette::PaletteAction::NewChat) => Some(
            Icon::new(IconName::Plus)
                .size(metrics.icon_small())
                .text_color(parts::subtle(cx))
                .into_any_element(),
        ),
        Some(crate::app::palette::PaletteAction::Settings) => Some(
            Icon::new(IconName::Settings)
                .size(metrics.icon_small())
                .text_color(parts::subtle(cx))
                .into_any_element(),
        ),
        Some(crate::app::palette::PaletteAction::Calls) => Some(
            Icon::new(ProductIcon::Phone)
                .size(metrics.icon_small())
                .text_color(parts::subtle(cx))
                .into_any_element(),
        ),
        Some(crate::app::palette::PaletteAction::Files) => Some(
            Icon::new(ProductIcon::FileText)
                .size(metrics.icon_small())
                .text_color(parts::subtle(cx))
                .into_any_element(),
        ),
        Some(crate::app::palette::PaletteAction::ConversationSearch) => Some(
            Icon::new(IconName::Search)
                .size(metrics.icon_small())
                .text_color(parts::subtle(cx))
                .into_any_element(),
        ),
        None => None,
    };

    div()
        .id(SharedString::from(format!("command-palette-row-{index}")))
        .debug_selector(move || format!("command-palette-row-{index}"))
        .mx(metrics.space_sm())
        .px(metrics.space_md())
        .py(metrics.space_sm())
        .rounded(metrics.radius_md())
        .flex()
        .items_center()
        .gap(metrics.space_md())
        .cursor_pointer()
        .when(selected, |el| el.bg(cx.theme().list_active))
        .when(!selected, |el| {
            let hover = cx.theme().list_hover;
            el.hover(move |s| s.bg(hover))
        })
        .on_click(move |_, window, cx| {
            entity.update(cx, |app, cx| {
                app.palette.selected = index;
                app.palette_accept(window, cx);
            });
        })
        .child(
            div()
                .w(metrics.avatar_inline())
                .h(metrics.avatar_inline())
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .map(|el| match action_icon {
                    Some(icon) => el.child(icon),
                    None => el.child(avatar),
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(metrics.text_small())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .truncate()
                        .child(row.title.clone()),
                )
                .child(
                    div()
                        .text_size(metrics.text_meta())
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(row.subtitle.clone()),
                ),
        )
        .when(row.unread > 0, |el| {
            el.child(
                div()
                    .flex_shrink_0()
                    .min_w(metrics.space_lg())
                    .px(metrics.space_xxs())
                    .py(metrics.space_xxs())
                    .rounded_full()
                    .bg(cx.theme().primary)
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().primary_foreground)
                    .child(if row.unread > 99 {
                        "99+".to_string()
                    } else {
                        row.unread.to_string()
                    }),
            )
        })
}
