//! One message in the timeline.
//!
//! The time and its tick sit inline at the end of the last line rather than
//! on a row of their own, which is what lets a one-word message be a
//! one-word bubble. Nothing here predicts a height: the list measures each row
//! as it lays it out, so a padding changed in this file cannot drift out of
//! step with a number kept elsewhere.

pub mod audio;
pub(crate) mod business;
mod media;
mod poll;
mod quote;
mod reactions;
mod system;

use crate::retained_image::RetainedImage;
use std::sync::Arc;

use gpui::{
    App, Entity, InteractiveElement, IntoElement, ObjectFit, ParentElement, RenderImage,
    SharedString, StatefulInteractiveElement as _, Styled, StyledImage as _, div, img,
    prelude::FluentBuilder as _,
};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::ContextMenuExt as _;
use gpui_component::{Icon, IconName, Sizable as _, h_flex, v_flex};

pub use audio::SPEEDS;
pub use system::render_encryption_notice;

use media::{MediaProps, render_media_content};
pub(crate) use media::{decode_sticker, sticker_payload_is_valid};
use poll::render_poll;
use quote::render_quote;
use reactions::{render_hover_actions, render_reaction_picker, render_reactions};

use crate::app::{
    BubbleIds, CopyMessage, DeleteSentMessage, EditMessage, OpenMessageInfo, OpenMessageLink,
    ReactToMessage, RegenerateLinkPreview, ReplyToMessage, RetryMessage, WhatsAppApp,
    can_delete_sent,
};
use crate::components::parts;
use crate::components::{Avatar, BubbleText, bubble_status_ticks, render_rich_text};
use crate::responsive::ResponsiveLayout;
use crate::theme::{ActiveProductTheme as _, Metrics};
use crate::utils::format_time_local;
use crate::video::VideoPlayerState;
use velo_core::ChatMessage;

/// Everything one bubble needs, gathered by the list.
pub struct BubbleProps {
    /// This row's element ids, formatted when the timeline was built.
    pub ids: BubbleIds,
    /// The sender's picture, where the daemon has resolved one for this
    /// group sender. `None` draws the silhouette, not a gap.
    pub sender_avatar: Option<String>,
    /// This row's text, parsed when the timeline was built. Travels with the
    /// row for the same reason the ids do: deriving it here is deriving it
    /// per frame for text that cannot change without a rebuild.
    pub text: BubbleText,
    /// Shared with the timeline's cache rather than copied out of it: a
    /// `ChatMessage` is four `String`s, a reaction map, a quote and a media
    /// handle, and the list builds one of these per visible row per frame.
    pub message: Arc<ChatMessage>,
    /// Which conversation this row is drawn in. A bubble does not carry it
    /// — the vote names the chat, so it travels in beside the message.
    pub chat_jid: SharedString,
    /// Current message index used when text spans multiple bubbles.
    pub selection_order: u64,
    /// Last vote requested here, not a confirmed vote.
    pub attempted_vote: Option<u32>,
    pub playing_message_id: Option<String>,
    pub is_group: bool,
    /// Whether the conversation is with your own number, which is what makes
    /// a sent message read the moment it lands.
    pub is_own_number: bool,
    /// Resolved while the list holds the app, so rendering never re-enters it.
    pub quoted_sender_is_self: bool,
    /// First message of a run by this author, which is what earns the sender
    /// name and the wider gap above.
    pub starts_run: bool,
    /// Whether the group's last bubble is this one. The avatar is drawn at
    /// the foot of the run, as the phone does, while the name stays on the
    /// first bubble at the top.
    pub ends_run: bool,
    /// Names of the people who have read this outgoing group message.
    pub read_by: Vec<String>,
    /// Whether the timeline just jumped to this row.
    pub highlighted: bool,
    /// This outgoing message's link card is still being fetched.
    pub link_preview_pending: bool,
    /// The decoded thumbnail for the link card, shared across frames.
    pub link_preview_image: Option<gpui::ImageSource>,
    pub video_player_state: Option<VideoPlayerState>,
    pub video_frame: Option<Arc<RenderImage>>,
    pub decoded_image: Option<RetainedImage>,
    /// Where this clip's playback is, when this clip is the one playing.
    ///
    /// Read out by the list rather than looked up here: the virtual list has
    /// already leased the app to build this row, and reading that entity a
    /// second time inside the row panics. Every value a bubble needs from the
    /// app comes through this struct for that reason.
    pub audio: Option<AudioProgress>,
    pub playback_speed: f32,
    /// Whether this message's media is being fetched right now.
    pub is_downloading: bool,
    /// Exact native file produced by this message's download, if still known.
    pub saved_document_path: Option<std::path::PathBuf>,
    /// Whether the quick-react strip is open under this bubble. Travels
    /// with the row for the reason the ids do: reading the app here would
    /// re-enter the entity the virtual list already leased to build it.
    pub reaction_picker_open: bool,
    /// Whether its downloaded audio is being decoded or retimed for playback.
    pub is_preparing: bool,
}

/// How far into the voice note the player is.
#[derive(Debug, Clone, Copy)]
pub struct AudioProgress {
    pub total_secs: f32,
    /// 0..=1 through the clip.
    pub fraction: f32,
    pub elapsed_secs: f32,
}

pub fn render_message_bubble(
    props: BubbleProps,
    entity: Entity<WhatsAppApp>,
    layout: ResponsiveLayout,
    cx: &App,
) -> impl IntoElement + use<> {
    let metrics = *layout.metrics();
    let ids = props.ids;
    let message = props.message;
    let sender_avatar = props.sender_avatar.clone();
    // A row nobody typed belongs to the conversation, not to a side of it.
    if let Some(notice) = &message.system {
        return div()
            .w_full()
            .flex()
            .justify_center()
            .pt(metrics.bubble_gap_authored())
            .child(system::render_system_row(
                notice,
                message.sender.clone(),
                message.id.clone(),
                entity,
                metrics,
                cx,
            ))
            .into_any_element();
    }
    let is_from_me = message.is_from_me;
    let message_id = message.id.clone();
    let bubble_id = ids.bubble.clone();
    let content = &props.text;
    let time: SharedString = format_time_local(&message.timestamp).into();
    let menu_message = message.clone();
    let status = message.delivery_in(props.is_own_number);
    let edited = message.edited && !message.revoked;
    let is_playing = props.playing_message_id.as_deref() == Some(message_id.as_str());
    let has_reactions = !message.reactions.is_empty();

    // A name if anyone has one, and the number if nobody does. The number is
    // drawn rather than stored: `sender_name` only ever gains a value, so a
    // row stamped with a number could never take the push name that arrives
    // after it, and everyone unknown would stay a number for the session.
    let sender_name: Option<SharedString> = (props.is_group && !is_from_me && props.starts_run)
        .then(|| SharedString::from(message.author_label().into_owned()));
    // The sender's own hue, so a group reads as a conversation between people
    // rather than a wall of one colour.
    let sender_hue = cx.product().speaker(&message.sender);

    let product = cx.product();
    // Media is not a message with a picture in it: it is the picture. The
    // bubble's own surface is what drew the coloured frame around every
    // photo, sticker and video — the padding kept the image inside it, and
    // the sent bubble's ultramarine read as a blue border. A row whose only
    // content is media therefore gets no surface and the smallest padding
    // the theme has; what remains is the wallpaper around a picture, which
    // is what every other messenger shows.
    //
    // Captions keep their bubble: the text still needs a ground to sit on,
    // and a caption without one is harder to read than the frame was to
    // remove.
    let media_only = message.media.is_some()
        && content.is_empty()
        && message.quoted.is_none()
        && message.poll.is_none()
        && message.business.is_none();
    // A photo with a caption is one message, not a caption inside a photo's
    // blue box. The picture keeps the wallpaper as its ground and the words
    // keep a bubble of their own, the way WhatsApp draws a captioned send.
    let captioned_media = message.media.is_some()
        && (!content.is_empty() || message.quoted.is_some())
        && message.poll.is_none();
    // An emoji on its own is not a sentence in a blue box: it is the message,
    // at emoji size, over the wallpaper. The sender name and avatar still
    // give it a side and an author.
    let emoji_only = message.media.is_none()
        && message.quoted.is_none()
        && message.poll.is_none()
        && message.business.is_none()
        && is_emoji_only(&message.content);
    let bubble_surface = if is_from_me {
        product.hsla(product.palette.message_sent)
    } else {
        product.hsla(product.palette.message_received)
    };
    let bubble_bg = if media_only || emoji_only || captioned_media {
        gpui::hsla(0., 0., 0., 0.)
    } else {
        bubble_surface
    };

    let retry_entity = entity.clone();
    let retry_id = message_id.clone();
    let failed = message.is_failed();
    // Whether there is anything to send again — which is not the same
    // question as whether the send failed, and drawing the button off the
    // latter offered a retry that had nothing to put on the wire.
    let can_retry = message.resend().is_some();

    // Right-click anywhere on the row, which is what a desktop reader reaches
    // for and the only route to these commands that does not require finding a
    // control that is invisible until the pointer is already over it.
    let menu_id = message_id.clone();
    let menu_text = message
        .business
        .as_ref()
        .map_or_else(|| message.content.clone(), |business| business.summary());
    let menu_failed = can_retry;
    let menu_jid = props.chat_jid.clone();
    let now_ms = wacore::time::now_millis();
    let menu_delete_for_me = can_delete_sent(&message, false, now_ms);
    // Anything with content that has reached the store: not a deleted
    // placeholder, not a system line, not ours still on its way out.
    let menu_forwardable = !message.revoked
        && message.system.is_none()
        && (!message.is_from_me || message.status.has_left_this_device());
    let menu_delete_for_everyone = can_delete_sent(&message, true, now_ms);
    // A refcount, not a rescan: the row's text already parsed these when the
    // timeline was built, and the targets were shared then.
    let menu_links = content.link_targets().clone();
    // The keyboard route to the same addresses, beside the menu entries
    // below: the inline ranges answer only the pointer and the menu opens on
    // right-click alone, so keyboard users tab to these instead.
    let link_buttons = menu_links.clone();

    div()
        .id(ids.row.clone())
        .w_full()
        // The row gives before the gutter does: a bubble at its maximum width
        // plus the action bar beside it is wider than a narrow pane, and
        // without this the overflow went out through the window's edge.
        .min_w_0()
        .when(props.highlighted, |el| {
            el.bg(cx.theme().primary.opacity(0.12))
                .rounded(metrics.radius_md())
        })
        .flex()
        .group(ids.group.clone())
        .map(|el| {
            if is_from_me {
                el.justify_end()
            } else {
                el.justify_start()
            }
        })
        .pt(if props.starts_run {
            metrics.bubble_gap_authored()
        } else {
            metrics.bubble_gap_grouped()
        })
        .items_end()
        .gap(metrics.space_md())
        // The action bar sits outside the bubble on the side the reader's eye
        // is not already on, so it never covers the message it acts upon.
        .when(is_from_me, |el| {
            el.child(
                div()
                    .flex_shrink_0()
                    .invisible()
                    .group_hover(ids.group.clone(), |s| s.visible())
                    .child(render_hover_actions(
                        &ids,
                        message_id.clone(),
                        message.content.clone(),
                        entity.clone(),
                        metrics,
                        cx,
                    )),
            )
        })
        // A group reads as people when the face is beside what they said.
        // The gutter is reserved on every row so a run's bubbles line up;
        // only the first row of a run draws a picture.
        .when(!is_from_me && props.is_group, |el| {
            el.child(render_sender_gutter(
                &message,
                sender_avatar,
                props.ends_run,
                entity.clone(),
                metrics,
                cx,
            ))
        })
        .child(
            v_flex()
                .min_w_0()
                .when(is_from_me, |el| el.items_end())
                .when(!is_from_me, |el| el.items_start())
                .child(
                    div()
                        .id(bubble_id)
                        .max_w(layout.max_bubble_width())
                        // The guarantee, not the fix: whatever a child does
                        // with an unbreakable token, a URL, or a caption, it
                        // is painted inside the bubble or not at all.
                        .overflow_hidden()
                        .px(if media_only || emoji_only || captioned_media {
                            metrics.space_xs()
                        } else {
                            metrics.bubble_padding_x()
                        })
                        .py(if media_only || emoji_only || captioned_media {
                            metrics.space_xs()
                        } else {
                            metrics.bubble_padding_y()
                        })
                        // Asymmetric corners: the tight one marks the side the
                        // message came from, so authorship survives even when
                        // the two bubble colours are close.
                        .rounded(metrics.radius_bubble())
                        .map(|el| {
                            if is_from_me {
                                el.rounded_br(metrics.radius_bubble_tail())
                            } else {
                                el.rounded_bl(metrics.radius_bubble_tail())
                            }
                        })
                        .bg(bubble_bg)
                        .when(failed, |el| el.border_1().border_color(cx.theme().danger))
                        .child(
                            v_flex()
                                .gap(metrics.space_md())
                                .children(sender_name.map(|name| {
                                    // Click a name to message that person,
                                    // as in WhatsApp: the way from "who
                                    // said that?" to a private reply.
                                    let person = message.sender.clone();
                                    let person_name = name.to_string();
                                    let dm = entity.clone();
                                    div()
                                        .id(SharedString::from(format!("sender-{message_id}")))
                                        .text_size(metrics.text_small())
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(sender_hue)
                                        .cursor_pointer()
                                        .hover(|style| style.underline())
                                        .tooltip({
                                            let tip = SharedString::from(
                                                crate::tr!(message_person, name.clone())
                                                    .to_string(),
                                            );
                                            move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(tip.clone())
                                                    .build(window, cx)
                                            }
                                        })
                                        .on_click(move |_, window, cx| {
                                            cx.stop_propagation();
                                            let person = person.clone();
                                            let person_name = person_name.clone();
                                            dm.update(cx, |app, cx| {
                                                app.message_person(
                                                    person,
                                                    Some(person_name),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        })
                                        .child(name)
                                }))
                                .children(if captioned_media {
                                    None
                                } else {
                                    message.quoted.as_ref().map(|quoted| {
                                        render_quote(
                                            quoted,
                                            props.quoted_sender_is_self,
                                            entity.clone(),
                                            metrics,
                                            cx,
                                        )
                                    })
                                })
                                .children(message.business.as_ref().map(|_| {
                                    business::render_header(
                                        content.business(),
                                        props.selection_order,
                                        metrics,
                                        cx,
                                    )
                                }))
                                .children(message.poll.as_ref().map(|poll| {
                                    render_poll(
                                        poll,
                                        props.chat_jid.clone(),
                                        &message_id,
                                        props.attempted_vote,
                                        None,
                                        entity.clone(),
                                        metrics,
                                        cx,
                                    )
                                }))
                                .when_some(message.media.clone(), |el, media_content| {
                                    render_media_content(
                                        el,
                                        media_content,
                                        message_id.clone(),
                                        is_playing,
                                        entity.clone(),
                                        MediaProps {
                                            chat_jid: props.chat_jid.clone(),
                                            video_player_state: props.video_player_state,
                                            video_frame: props.video_frame.clone(),
                                            decoded_image: props.decoded_image.clone(),
                                            audio: props.audio,
                                            playback_speed: props.playback_speed,
                                            listened: audio::played_label(
                                                message.is_from_me,
                                                message.status,
                                            ),
                                            is_downloading: props.is_downloading,
                                            is_preparing: props.is_preparing,
                                            saved_document_path: props.saved_document_path.clone(),
                                            max_media_size: layout.max_media_size(),
                                            sticker_size: layout.sticker_size(),
                                        },
                                        cx,
                                    )
                                })
                                // Text and its trailing meta share one line
                                // box, so a short message is a short bubble.
                                .child(
                                    div()
                                        .flex()
                                        .when(captioned_media, |row| {
                                            row.flex_col()
                                                .gap(metrics.space_xxs())
                                                .px(metrics.bubble_padding_x())
                                                .py(metrics.bubble_padding_y())
                                                .rounded(metrics.radius_bubble())
                                                .map(|row| {
                                                    if is_from_me {
                                                        row.rounded_br(metrics.radius_bubble_tail())
                                                    } else {
                                                        row.rounded_bl(metrics.radius_bubble_tail())
                                                    }
                                                })
                                                .bg(bubble_surface)
                                        })
                                        .when(!captioned_media, |row| {
                                            row.flex_wrap().items_end().gap(metrics.space_md())
                                        })
                                        .children(if captioned_media {
                                            message.quoted.as_ref().map(|quoted| {
                                                render_quote(
                                                    quoted,
                                                    props.quoted_sender_is_self,
                                                    entity.clone(),
                                                    metrics,
                                                    cx,
                                                )
                                            })
                                        } else {
                                            None
                                        })
                                        .when(message.poll.is_none() && !content.is_empty(), |el| {
                                            el.child(
                                                div()
                                                    .flex_1()
                                                    // A flex item's minimum is
                                                    // its *min-content* width
                                                    // unless told otherwise,
                                                    // and for one unbroken
                                                    // 60-character word that
                                                    // is the whole word. The
                                                    // bubble's max width then
                                                    // clamped the box while
                                                    // the text kept drawing
                                                    // past it, across the
                                                    // conversation and out of
                                                    // the window.
                                                    .min_w_0()
                                                    .text_size(if emoji_only {
                                                        metrics.text_body() * 2.6
                                                    } else {
                                                        metrics.text_body()
                                                    })
                                                    // Inter Regular at ~1.4: the
                                                    // reading measure Signal uses
                                                    // for message text, tighter
                                                    // than gpui's default golden
                                                    // ratio, which spread long
                                                    // messages into ruled paper.
                                                    .font_weight(gpui::FontWeight::NORMAL)
                                                    .line_height(gpui::relative(if emoji_only {
                                                        1.1
                                                    } else {
                                                        1.42
                                                    }))
                                                    .when(emoji_only, |el| {
                                                        el.font_family(
                                                            crate::platform::fonts::emoji_family(
                                                                cx,
                                                            ),
                                                        )
                                                    })
                                                    .text_color(cx.theme().foreground)
                                                    .child(render_rich_text(
                                                        content,
                                                        ids.selection_key.clone(),
                                                        props
                                                            .selection_order
                                                            .saturating_mul(3)
                                                            .saturating_add(1),
                                                        cx,
                                                    )),
                                            )
                                        })
                                        .child(render_meta(
                                            time,
                                            status,
                                            is_from_me,
                                            edited,
                                            ids.edited.clone(),
                                            metrics,
                                            cx,
                                        )),
                                )
                                .children(message.business.as_ref().map(|business| {
                                    business::render_footer(
                                        business,
                                        content.business(),
                                        props.selection_order,
                                        metrics,
                                        cx,
                                    )
                                })),
                        ),
                )
                .children(if props.link_preview_pending {
                    Some(
                        render_link_preview_skeleton(layout.max_bubble_width(), metrics, cx)
                            .into_any_element(),
                    )
                } else {
                    message.link_preview.clone().map(|preview| {
                        render_link_preview(
                            preview,
                            props.link_preview_image.clone(),
                            layout.max_bubble_width(),
                            metrics,
                            cx,
                        )
                        .into_any_element()
                    })
                })
                .when(!props.read_by.is_empty(), |el| {
                    el.child(
                        div()
                            .mt(metrics.space_xxs())
                            .text_size(metrics.text_micro())
                            .text_color(parts::subtle(cx))
                            .child(read_by_label(&props.read_by)),
                    )
                })
                .when(!link_buttons.is_empty(), |el| {
                    // Opening an address is a command, so each one is a
                    // `Button` — that is what carries focus and keyboard
                    // activation, which the inline ranges never have. One
                    // per address, naming what it opens, like the menu
                    // entries below.
                    // Shown on hover, like the other per-message actions:
                    // always-visible, they read as a stray line of text
                    // under every message with a link in it.
                    el.child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .max_w(layout.max_bubble_width())
                            .gap(metrics.space_xxs())
                            .mt(metrics.space_xs())
                            .invisible()
                            .group_hover(ids.group.clone(), |s| s.visible())
                            .children(link_buttons.iter().enumerate().map(|(ix, target)| {
                                let url = target.clone();
                                Button::new(SharedString::from(format!(
                                    "open-link-{message_id}-{ix}"
                                )))
                                .label(crate::tr!(open_target, target))
                                .ghost()
                                .xsmall()
                                .tooltip(crate::tr!(open_target, target))
                                .cursor_pointer()
                                .on_click(move |_, _, cx| {
                                    cx.open_url(&url);
                                })
                            })),
                    )
                })
                .when(can_retry, |el| {
                    // Sending again is a command, not a surface: a styled div
                    // has no keyboard activation, so a failed message would
                    // only be recoverable with a pointer.
                    el.child(
                        div().mt(metrics.space_xs()).child(
                            Button::new(ids.retry.clone())
                                .label(crate::tr!(NotSentRetry))
                                .ghost()
                                .danger()
                                .xsmall()
                                .tooltip(crate::tr!(SendThisMessageAgain))
                                .cursor_pointer()
                                .on_click(move |_, window, cx| {
                                    retry_entity.update(cx, |app, cx| {
                                        app.retry_send(&retry_id, window, cx)
                                    });
                                }),
                        ),
                    )
                })
                .when(props.reaction_picker_open, |el| {
                    el.child(crate::motion::pop_in(
                        div().child(render_reaction_picker(
                            &message_id,
                            entity.clone(),
                            metrics,
                            cx,
                        )),
                        SharedString::from(format!("reaction-picker-in-{message_id}")),
                    ))
                })
                .when(has_reactions, |el| {
                    el.child(render_reactions(
                        message.reactions.clone(),
                        is_from_me,
                        metrics,
                        cx,
                    ))
                }),
        )
        .when(!is_from_me, |el| {
            el.child(
                div()
                    .flex_shrink_0()
                    .invisible()
                    .group_hover(ids.group.clone(), |s| s.visible())
                    .child(render_hover_actions(
                        &ids,
                        message_id.clone(),
                        message.content.clone(),
                        entity,
                        metrics,
                        cx,
                    )),
            )
        })
        // Last in the chain: the wrapper is no longer a `Div`, so anything
        // styled after this would have nowhere to go.
        .context_menu(move |menu, _window, _cx| {
            // The same emojis the hover button's strip offers, one item
            // each: a strip is a pointer surface, and this menu is the
            // route keyboard and assistive-technology users take to the
            // same command. Tapping ours again takes it back, exactly as
            // the strip does.
            let mut menu = menu;
            if crate::app::editing::can_edit_text(&menu_message, wacore::time::now_millis()) {
                menu = menu.menu(
                    crate::tr!(Edit),
                    Box::new(EditMessage {
                        id: menu_id.clone().into(),
                    }),
                );
            }
            for emoji in WhatsAppApp::QUICK_REACTIONS {
                let react_id = menu_id.clone();
                menu = menu.menu(
                    crate::tr!(react_emoji, emoji),
                    Box::new(ReactToMessage {
                        id: react_id.into(),
                        emoji: SharedString::from(emoji),
                    }),
                );
            }
            let menu = menu.menu(
                crate::tr!(Reply),
                Box::new(ReplyToMessage {
                    id: menu_id.clone().into(),
                }),
            );
            let menu = if menu_forwardable {
                menu.menu(
                    "Forward",
                    Box::new(crate::app::forward::ForwardMessage {
                        jid: menu_jid.clone(),
                        id: menu_id.clone().into(),
                    }),
                )
            } else {
                menu
            };
            let menu = if is_from_me {
                menu.menu(
                    "Message info",
                    Box::new(OpenMessageInfo {
                        jid: menu_jid.clone(),
                        id: menu_id.clone().into(),
                    }),
                )
            } else {
                menu
            };
            let menu = if menu_text.is_empty() {
                menu
            } else {
                menu.menu(
                    crate::tr!(CopyText),
                    Box::new(CopyMessage {
                        text: menu_text.clone().into(),
                    }),
                )
            };
            // One command per address, naming what it opens: the inline
            // ranges answer only the pointer, and a menu item is activatable
            // once the menu is open, which those ranges never are.
            let mut menu = menu;
            // A card fetched when the thumbnail cap was lower — or one the
            // sender's client shrunk — can be replaced here without touching
            // the message on the wire.
            let preview_target = menu_message
                .link_preview
                .as_ref()
                .map(|preview| preview.url.clone())
                .or_else(|| crate::platform::link_preview::first_url(&menu_message.content));
            if let Some(url) = preview_target {
                menu = menu.menu(
                    "Regenerate preview",
                    Box::new(RegenerateLinkPreview {
                        jid: menu_jid.clone(),
                        id: menu_id.clone().into(),
                        url: url.into(),
                    }),
                );
            }
            for target in menu_links.iter() {
                menu = menu.menu(
                    crate::tr!(open_target, target),
                    Box::new(OpenMessageLink {
                        url: target.clone(),
                    }),
                );
            }
            if menu_delete_for_everyone {
                menu = menu.separator().menu(
                    crate::tr!(DeleteForEveryone),
                    Box::new(DeleteSentMessage {
                        jid: menu_jid.clone(),
                        id: menu_id.clone().into(),
                        for_everyone: true,
                    }),
                );
            }
            if menu_delete_for_me {
                menu = menu.menu(
                    crate::tr!(DeleteForMe),
                    Box::new(DeleteSentMessage {
                        jid: menu_jid.clone(),
                        id: menu_id.clone().into(),
                        for_everyone: false,
                    }),
                );
            }
            if menu_failed {
                menu.separator().menu(
                    crate::tr!(SendAgain),
                    Box::new(RetryMessage {
                        id: menu_id.clone().into(),
                    }),
                )
            } else {
                menu
            }
        })
        .into_any_element()
}

/// Whether a message is nothing but emoji.
///
/// WhatsApp draws these without a bubble and much larger: a single face is
/// not a sentence, and a blue box around one reads as a mistake. Joiners,
/// variation selectors, skin tones and keycap marks belong to the cluster
/// they follow; digits and `#`/`*` count only inside a keycap sequence, so
/// "2024" never qualifies. Three emoji is the cutoff — past that the line is
/// a message again and wraps like one.
fn is_emoji_only(content: &str) -> bool {
    let has_keycap = content.contains('\u{20E3}');
    let mut clusters = 0usize;
    let mut joined = false;
    for ch in content.chars() {
        if ch.is_whitespace() {
            joined = false;
            continue;
        }
        let value = ch as u32;
        // A zero-width joiner glues the next scalar onto the cluster that
        // just ended. Selectors, keycaps and skin tones belong to the
        // previous scalar and start nothing on their own.
        if value == 0x200D {
            joined = true;
            continue;
        }
        if matches!(value, 0xFE0F | 0xFE0E | 0x20E3) || (0x1F3FB..=0x1F3FF).contains(&value) {
            continue;
        }
        let member = is_emoji_scalar(value) || (has_keycap && matches!(ch, '0'..='9' | '#' | '*'));
        if !member {
            return false;
        }
        if !joined {
            clusters += 1;
        }
        joined = false;
    }
    (1..=3).contains(&clusters)
}

/// The scalar values Unicode assigns to emoji presentation by default.
fn is_emoji_scalar(value: u32) -> bool {
    matches!(
        value,
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x21AA
            | 0x231A..=0x231B
            | 0x2328
            | 0x23CF
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x24C2
            | 0x25AA..=0x25FE
            | 0x2600..=0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
    )
}

/// The time and, on our own messages, its tick.
/// The face beside a group message, or the empty column that keeps a run
/// aligned with the message it continues.
fn render_sender_gutter(
    message: &ChatMessage,
    avatar: Option<String>,
    show: bool,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let size = metrics.avatar_inline();
    let name = message.author_label().into_owned();
    div()
        .w(size)
        .flex_shrink_0()
        .flex()
        .items_end()
        .justify_center()
        .when(show, |el| {
            let person = message.sender.clone();
            let click_name = name.clone();
            el.child(
                div()
                    .id(SharedString::from(format!("sender-avatar-{}", message.id)))
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        let person = person.clone();
                        let click_name = click_name.clone();
                        entity.update(cx, |app, cx| {
                            app.message_person(person, Some(click_name), window, cx);
                        });
                    })
                    .child(
                        Avatar::new(message.sender.clone(), &name, size)
                            .picture(avatar, None)
                            .on(cx.theme().background),
                    ),
            )
        })
}

/// The card WhatsApp draws under a message that carries a link.
///
/// The card's shape while its page is still being read.
///
/// The bubble used to look finished and then grow a card a second later,
/// which read as "there is no preview" rather than "this is loading". The
/// bars occupy the same height the real card will, so the message does not
/// jump when the fields arrive.
fn render_link_preview_skeleton(
    max_width: gpui::Pixels,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    div()
        .id("link-preview-loading")
        .debug_selector(|| "link-preview-loading".into())
        .mt(metrics.space_xxs())
        .w_full()
        .max_w(max_width)
        .overflow_hidden()
        .rounded(metrics.radius_md())
        .bg(cx.theme().secondary)
        .border_1()
        .border_color(cx.theme().border)
        .flex()
        .flex_col()
        .child(
            div()
                .w_full()
                .px(metrics.space_sm())
                .py(metrics.space_xs())
                .flex()
                .flex_col()
                .gap(metrics.space_xs())
                .child(
                    div()
                        .w(metrics.space_xxxl() * 3.0)
                        .h(metrics.space_xs())
                        .rounded_full()
                        .bg(cx.theme().border),
                )
                .child(
                    div()
                        .w_full()
                        .h(metrics.space_sm())
                        .rounded_full()
                        .bg(cx.theme().border),
                ),
        )
        .child(
            div()
                .w_full()
                .aspect_ratio(1.9)
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .child(
                    Icon::new(IconName::LoaderCircle)
                        .size(metrics.icon())
                        .text_color(cx.theme().muted_foreground),
                ),
        )
}

/// The sender's client already fetched the page; this side only keeps the
/// four fields it sent. A card without an address is not drawn at all, so the
/// button that opens it always has somewhere to go.
fn render_link_preview(
    preview: velo_core::LinkPreview,
    image: Option<gpui::ImageSource>,
    max_width: gpui::Pixels,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let url = preview.url.clone();
    let host = SharedString::from(preview.host().to_string());
    let site = preview
        .site_name
        .clone()
        .filter(|site| !site.trim().is_empty())
        .unwrap_or_else(|| host.to_string());
    let title = preview
        .title
        .clone()
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| host.to_string());
    let description = preview
        .description
        .clone()
        .filter(|description| !description.is_empty());
    let has_image = image.is_some();
    let monogram = host
        .chars()
        .next()
        .map(|letter| letter.to_uppercase().to_string())
        .unwrap_or_else(|| "#".to_string());
    div()
        .id("link-preview")
        .debug_selector(|| "link-preview".into())
        .mt(metrics.space_xxs())
        .w_full()
        .max_w(max_width)
        .overflow_hidden()
        .rounded(metrics.radius_md())
        .bg(cx.theme().secondary)
        .border_1()
        .border_color(cx.theme().border)
        .cursor_pointer()
        .on_click(move |_, _, cx| cx.open_url(&url))
        .flex()
        .flex_col()
        .child(
            div()
                .w_full()
                .min_w_0()
                .px(metrics.space_sm())
                .py(metrics.space_xs())
                .flex()
                .flex_col()
                .gap(metrics.space_xxs())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.space_xs())
                        // A page with no picture - a private repository, a
                        // 404, a site that never published one - still gets a
                        // visual mark instead of a card that looks unfinished.
                        .when(!has_image, |row| {
                            row.child(
                                div()
                                    .size(metrics.space_xl())
                                    .flex_shrink_0()
                                    .rounded(metrics.radius_sm())
                                    .bg(cx.theme().primary.opacity(0.14))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(metrics.text_micro())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().primary)
                                    .child(monogram),
                            )
                        })
                        .child(
                            div()
                                .min_w_0()
                                .text_size(metrics.text_micro())
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(site),
                        ),
                )
                .child(
                    div()
                        .text_size(metrics.text_small())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .truncate()
                        .child(title),
                )
                .children(description.map(|description| {
                    div()
                        .text_size(metrics.text_meta())
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(description)
                })),
        )
        .children(image.map(|image| {
            div()
                .w_full()
                .aspect_ratio(1.9)
                .flex_shrink_0()
                .child(img(image).size_full().object_fit(ObjectFit::Cover))
        }))
}

/// "Read by Ana, Luis and 2 more" without listing a whole group.
fn read_by_label(names: &[String]) -> String {
    let visible = names.iter().take(3).cloned().collect::<Vec<_>>();
    let remaining = names.len().saturating_sub(visible.len());
    if remaining == 0 {
        format!("Read by {}", visible.join(", "))
    } else {
        format!("Read by {} and {remaining} more", visible.join(", "))
    }
}

fn render_meta(
    time: SharedString,
    status: Option<velo_core::MessageStatus>,
    is_from_me: bool,
    edited: bool,
    edited_id: SharedString,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let product = cx.product();
    // On the sent bubble the muted ink is unreadable against the brand
    // colour, so the meta lifts out of the bubble's own hue instead.
    let colour = if is_from_me {
        product.hsla(
            product
                .palette
                .message_sent
                .mix(product.palette.foreground, 0.55),
        )
    } else {
        parts::subtle(cx)
    };

    h_flex()
        .flex_shrink_0()
        .items_center()
        .gap(metrics.space_xs())
        .children(edited.then(|| {
            let selector = edited_id.clone();
            div()
                .id(edited_id)
                .debug_selector(move || selector.to_string())
                .text_size(metrics.text_micro())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(colour)
                .child(crate::tr!(Edited))
        }))
        .child(
            parts::meta()
                .text_size(metrics.text_micro())
                .text_color(colour)
                .child(time),
        )
        .children(status.map(|status| bubble_status_ticks(status, metrics.icon_small(), cx)))
}

#[cfg(test)]
mod tests {
    use super::is_emoji_only;

    #[test]
    fn only_emoji_earn_the_bubbleless_treatment() {
        for emoji in ["👍", "😂😂", "❤️", "👨‍👩‍👧", "🇪🇸", "1️⃣", "✌🏽"]
        {
            assert!(is_emoji_only(emoji), "{emoji} is emoji");
        }
        for text in ["hola", "👍 ok", "2024", "#1", ":)", "👍👍👍👍", "  ", ""] {
            assert!(!is_emoji_only(text), "{text:?} is not emoji-only");
        }
    }
}
