//! Isolated input area component with its own Entity and render cycle.
//!
//! This component is designed for performance: when the user types,
//! only this component re-renders, NOT the parent app.

use std::sync::Arc;
use std::time::Duration;

use wacore::time::Instant;

use gpui::{
    App, Entity, EntityInputHandler as _, EventEmitter, Focusable as _, SharedString,
    StyledImage as _, Subscription, Task, WeakEntity, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants},
    input::{Enter, Input, InputEvent, InputState, Paste, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
};

use crate::components::{Avatar, ProductIcon, parts};
use crate::platform::picker::AttachmentCategory;
use crate::theme::{ActiveProductTheme as _, Metrics};

/// Events emitted by the input area to communicate with the parent app.
#[derive(Clone, Debug)]
pub enum InputAreaEvent {
    /// User wants to send the current message.
    ///
    /// `text` is what the composer showed, for the local bubble; `wire_text`
    /// has every `@name` turned back into the address WhatsApp marks, which
    /// is what the recipient resolves against `mentions`.
    SendMessage {
        text: String,
        wire_text: String,
        mentions: Vec<String>,
        /// The card the composer already read for the first address in the
        /// text, when it finished in time. The send path reuses it instead of
        /// reading the page twice.
        link_preview: Option<velo_core::LinkPreview>,
    },
    /// User picked an attachment category from the composer menu.
    ///
    /// Which conversation is the app's to know, and the files are chosen
    /// after the press — a dialog the composer neither owns nor waits for.
    AttachFiles(AttachmentCategory),
    /// Ask the parent to paste clipboard media after it reserves the
    /// incoming-file slot.
    PasteMedia,
    /// User started PTT recording
    StartRecording,
    /// User stopped PTT recording (send the audio)
    StopRecording,
    /// Typing indicator: user started typing
    StartedTyping,
    /// Typing indicator: user stopped typing (timeout)
    StoppedTyping,
    /// User discarded the recording instead of sending it.
    CancelRecording,
    /// User dropped the reply they were composing.
    CancelReply,
    CancelEdit,
    /// A GIF chosen in the picker, already downloaded as the MP4 WhatsApp
    /// plays. The app puts it through the same confirmation as a pasted file.
    SendGif {
        bytes: Arc<Vec<u8>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerTab {
    Emoji,
    Gif,
}

/// One KLIPY result, with the animated preview once it has been fetched.
#[derive(Clone)]
struct GifPreview {
    result: crate::platform::gif::GifResult,
    image: Option<crate::retained_image::RetainedImage>,
    animated: bool,
}

/// Typing indicator state with debouncing.
#[derive(Default)]
enum TypingState {
    #[default]
    Idle,
    /// Currently typing - stores the instant of the last keystroke
    Composing(Instant),
}

/// Timeout before sending "paused" after typing stops (matches WhatsApp Web)
const TYPING_PAUSED_TIMEOUT: Duration = Duration::from_millis(2500);
/// How often the typing monitor checks for timeout
const TYPING_MONITOR_INTERVAL: Duration = Duration::from_millis(500);

/// The message being replied to, while it is being composed.
#[derive(Clone, Debug)]
pub struct ReplyDraft {
    pub message_id: String,
    pub sender: String,
    pub sender_name: String,
    /// A display role, never a translated name stored in the outgoing quote.
    pub sender_is_self: bool,
    pub preview: String,
    /// What the message being answered *is*, so the quote sent to the other
    /// side is a photo rather than the word "Photo".
    ///
    /// The preview is a label a human reads; this is what the recipient's
    /// client draws. Dropped here, the kind-aware quote builder in the
    /// session had nothing to be aware of.
    pub kind: Option<velo_core::QuotedKind>,
}

impl ReplyDraft {
    pub fn sender_label(&self, language: crate::i18n::Language) -> &str {
        if self.sender_is_self {
            language.text(crate::i18n::TextKey::You)
        } else {
            &self.sender_name
        }
    }
}

impl From<ReplyDraft> for velo_core::QuotedMessage {
    /// One conversion, so the two send paths cannot disagree about what a
    /// reply quotes. They each built this by hand and both hard-coded the
    /// kind away, which left the kind-aware quote builder in the session
    /// with nothing to work from.
    fn from(draft: ReplyDraft) -> Self {
        Self {
            message_id: draft.message_id,
            sender: draft.sender,
            sender_name: draft.sender_name,
            preview: draft.preview,
            kind: draft.kind,
        }
    }
}

/// One person the composer may `@`-mention.
///
/// Built by the app from the group roster: it is the roster that knows who is
/// in the group, and the app that knows what each of them is called and which
/// picture to draw. The composer only filters and inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionMember {
    /// The address as the group calls them, which is what goes on the wire.
    pub jid: String,
    /// What the picker draws: the saved name, their push name, an `@handle`,
    /// or the number when nothing else is known.
    pub label: String,
    /// Cache key of the avatar, when one has arrived.
    pub picture: Option<String>,
    /// The row that mentions the whole group.
    pub is_all: bool,
}

impl MentionMember {
    /// What the reader sees after picking them.
    fn display_token(&self) -> String {
        if self.is_all {
            "@all ".to_string()
        } else {
            format!("@{} ", self.label)
        }
    }

    /// What the wire must carry in the text: `@` plus the user part, which is
    /// how WhatsApp marks the token. `@all` has no user part and stays as it
    /// is; the recipients are named in the mention list.
    fn wire_token(&self) -> Option<String> {
        if self.is_all {
            return None;
        }
        let user = self
            .jid
            .split('@')
            .next()
            .unwrap_or(&self.jid)
            .split(':')
            .next()
            .unwrap_or(&self.jid);
        Some(format!("@{user}"))
    }
}

/// The `@` token being typed, and where it starts.
#[derive(Clone, Debug)]
struct MentionQuery {
    /// Byte offset of the `@` in the composer text.
    start: usize,
    /// What follows it up to the caret.
    text: String,
}

/// Read the `@` token that ends at `cursor`, if the caret is inside one.
///
/// The `@` has to begin a word and the token has to be one unbroken run: a
/// space or a second `@` ends it, which is what makes "mail me at a@b" not
/// open a member picker. The length cap keeps a pasted wall of text from
/// matching anything.
fn mention_query_at(text: &str, cursor: usize) -> Option<MentionQuery> {
    let cursor = cursor.min(text.len());
    let head = text.get(..cursor)?;
    let start = head.rfind('@')?;
    let starts_a_word = start == 0
        || head[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
    if !starts_a_word {
        return None;
    }
    let query = &head[start + 1..];
    if query.contains(char::is_whitespace) || query.contains('@') || query.chars().count() > 24 {
        return None;
    }
    Some(MentionQuery {
        start,
        text: query.to_string(),
    })
}

/// Turn the labels the composer drew back into what the wire expects.
///
/// The reader sees `@Ana`; WhatsApp marks the token by the address's user
/// part and resolves the display name on the other side. `@all` has no user
/// part, so its text stays and the mention list names everyone instead. A
/// label whose token the reader deleted is not mentioned.
fn wire_mentions(
    text: &str,
    inserted: &[MentionMember],
    everyone: &[String],
) -> (String, Vec<String>) {
    if inserted.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let mut wire = text.to_string();
    let mut jids: Vec<String> = Vec::new();
    let mut ordered: Vec<&MentionMember> = inserted.iter().collect();
    // Longest label first: `@Ana Maria` must not be consumed as `@Ana`
    // followed by the rest of her name.
    ordered.sort_by_key(|member| std::cmp::Reverse(member.label.len()));
    for member in ordered {
        if member.is_all {
            if wire.contains("@all") {
                for jid in everyone {
                    if !jids.contains(jid) {
                        jids.push(jid.clone());
                    }
                }
            }
            continue;
        }
        let token = format!("@{}", member.label);
        let Some(position) = wire.find(&token) else {
            continue;
        };
        let after = wire[position + token.len()..].chars().next();
        if after.is_some_and(|next| {
            !(next.is_whitespace() || matches!(next, ',' | '.' | '!' | '?' | ';' | ':' | ')'))
        }) {
            continue;
        }
        if let Some(wire_token) = member.wire_token() {
            wire.replace_range(position..position + token.len(), &wire_token);
        }
        if !jids.contains(&member.jid) {
            jids.push(member.jid.clone());
        }
    }
    (wire, jids)
}

/// How far the field grows before it starts scrolling instead.
///
/// Five lines is the design's number, and it is about where a composer stops
/// being a composer: past that the message wants the whole pane, not a
/// growing strip pushing the conversation off the top.
const COMPOSER_MIN_ROWS: usize = 1;
const COMPOSER_MAX_ROWS: usize = 5;

/// Isolated input area view with its own render cycle.
/// When the user types, only this component re-renders.
pub struct InputAreaView {
    /// The field itself. A textarea rather than a single line: a message is
    /// not always one, and the design asks it to grow with what is typed.
    input: Entity<TextareaState>,
    emoji_open: bool,
    picker_tab: PickerTab,
    emoji_search: Entity<InputState>,
    gif_search: Entity<InputState>,
    gif_results: Vec<GifPreview>,
    gif_loading: bool,
    gif_error: Option<String>,
    gif_task: Option<Task<()>>,
    /// The first address in the composer and the card read for it.
    ///
    /// Prefetched while the person is still typing, so pressing send can use
    /// a card that is already in hand instead of racing a second fetch.
    draft_link_url: Option<String>,
    draft_link_preview: Option<velo_core::LinkPreview>,
    draft_link_pending: bool,
    /// Debounce before the address is read.
    link_probe_task: Option<Task<()>>,
    /// The read itself, kept so a changed address cancels the old one.
    link_fetch_task: Option<Task<()>>,
    language: crate::i18n::Language,
    /// Whether PTT recording is active
    is_recording: bool,
    editing: bool,
    edit_pending: bool,
    /// When the current recording started, for the elapsed counter.
    recording_started_at: Option<Instant>,
    /// Most recent input level, 0..=1, for the live meter.
    level: f32,
    /// The message being replied to, if any.
    reply: Option<ReplyDraft>,
    /// The slot height the layout allotted. `None` before the first layout
    /// pass, where the metrics' default is the right answer.
    height: Option<gpui::Pixels>,
    /// Pointer target size for this breakpoint — bigger on touch.
    touch_target: Option<gpui::Pixels>,
    /// Typing indicator state
    typing_state: TypingState,
    /// Task that monitors typing state
    #[allow(dead_code)]
    typing_monitor_task: Option<Task<()>>,
    /// Whether a voice note can be recorded here at all.
    ///
    /// Asked once rather than per frame: on the web the answer is whether the
    /// browser has an Opus encoder, which is a `AudioEncoder` constructed,
    /// configured and closed, real work, on every render of the composer,
    /// for a value that cannot change while the tab is open.
    can_record: bool,
    /// Everyone the `@` picker can offer, from the open group's roster.
    mention_members: Vec<MentionMember>,
    /// The addresses `@all` stands for: the group, minus this account.
    mention_everyone: Vec<String>,
    /// The `@` token under the caret, when the picker is open.
    mention_query: Option<MentionQuery>,
    /// Which filtered row the keyboard is on.
    mention_index: usize,
    /// Members whose display token was inserted, so send can turn the label
    /// back into the address WhatsApp marks and list them as mentioned.
    inserted_mentions: Vec<MentionMember>,
    /// Scroll position of the member list, so arrow navigation keeps the
    /// highlighted row visible without moving the conversation behind it.
    mention_scroll: gpui::ScrollHandle,
    /// Intercepts the keys the picker owns while it is open.
    ///
    /// A textarea binds Up/Down/Enter to its own actions, and key listeners
    /// run only after those bindings, so the picker has to see the stroke
    /// earlier than a listener would. The subscription also ends with the
    /// view, which a detached interceptor would not.
    _mention_keys: Subscription,
}

impl EventEmitter<InputAreaEvent> for InputAreaView {}

impl InputAreaView {
    pub fn init_bindings(cx: &mut App) {
        // Only this textarea treats Control+Enter as a newline. Other fields
        // retain the component library's secondary-confirm shortcut.
        cx.bind_keys([gpui::KeyBinding::new(
            "ctrl-enter",
            Enter {
                secondary: false,
                shift: true,
            },
            Some("MessageComposer > Input"),
        )]);
    }

    /// Create a new input area view
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Enter sends and Shift+Enter breaks the line, which is what
        // `submit_on_enter` means here; the field grows from one line to five
        // and scrolls past that rather than pushing the timeline off screen.
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(COMPOSER_MIN_ROWS, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
                .placeholder(crate::tr!(TypeAMessage))
        });

        // Subscribe to input events (for Enter key to send, etc.)
        cx.subscribe_in(&input, window, Self::handle_input_event)
            .detach();

        let emoji_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji…"));
        cx.subscribe(&emoji_search, |_, _, _: &InputEvent, cx| cx.notify())
            .detach();
        let gif_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search KLIPY"));
        cx.subscribe(&gif_search, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.queue_gif_search(cx);
            }
        })
        .detach();
        let mention_owner = cx.entity().downgrade();
        let mention_keys = cx.intercept_keystrokes(move |event, window, cx| {
            let key = event.keystroke.key.as_str();
            if !matches!(key, "up" | "down" | "enter" | "tab" | "escape") {
                return;
            }
            let Some(view) = mention_owner.upgrade() else {
                return;
            };
            let handled = view.update(cx, |view, cx| {
                if view.mention_query.is_none() || !view.focus_handle(cx).is_focused(window) {
                    return false;
                }
                match key {
                    "up" => {
                        view.mention_index = view.mention_index.saturating_sub(1);
                        view.scroll_mention_into_view();
                    }
                    "down" => {
                        let last = view.mention_results().len().saturating_sub(1);
                        view.mention_index = (view.mention_index + 1).min(last);
                        view.scroll_mention_into_view();
                    }
                    "enter" | "tab" => view.accept_mention(window, cx),
                    "escape" => view.close_mentions(cx),
                    _ => return false,
                }
                cx.notify();
                true
            });
            if handled {
                cx.stop_propagation();
            }
        });
        Self {
            emoji_open: false,
            picker_tab: PickerTab::Emoji,
            emoji_search,
            gif_search,
            gif_results: Vec::new(),
            gif_loading: false,
            gif_error: None,
            gif_task: None,
            draft_link_url: None,
            draft_link_preview: None,
            draft_link_pending: false,
            link_probe_task: None,
            link_fetch_task: None,
            input,
            language: crate::i18n::Language::current(),
            is_recording: false,
            editing: false,
            edit_pending: false,
            recording_started_at: None,
            level: 0.0,
            reply: None,
            height: None,
            touch_target: None,
            typing_state: TypingState::default(),
            typing_monitor_task: None,
            // Whether the browser has an Opus encoder, which is the one
            // thing that can make this control a promise it cannot keep.
            // Where it does, every arrangement a page can be in has somewhere
            // to send what it encodes: a daemon over the bridge, the tab
            // holding the account, or the page's own session.
            can_record: velo_audio::can_record(),
            mention_members: Vec::new(),
            mention_everyone: Vec::new(),
            mention_query: None,
            mention_index: 0,
            inserted_mentions: Vec::new(),
            mention_scroll: gpui::ScrollHandle::new(),
            _mention_keys: mention_keys,
        }
    }

    /// Handle input events (Enter, Change, etc.)
    fn handle_input_event(
        &mut self,
        _input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::PressEnter { shift: false, .. } => {
                if self.mention_query.is_some() {
                    self.accept_mention(window, cx);
                } else {
                    self.submit_input(window, cx);
                }
            }
            InputEvent::Change => {
                // Handle typing indicator - minimal work, no notify
                self.on_keystroke(cx);
                self.refresh_mention_query(cx);
                self.queue_draft_link_probe(cx);
            }
            _ => {}
        }
    }

    fn paste_image(&mut self, cx: &mut Context<Self>) {
        cx.emit(InputAreaEvent::PasteMedia);
    }

    /// Handle a keystroke - updates typing state
    /// Does NOT call cx.notify() to avoid triggering parent re-renders
    fn on_keystroke(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();

        match self.typing_state {
            TypingState::Idle => {
                // First keystroke - start typing
                self.typing_state = TypingState::Composing(now);
                // Emit event to parent (they handle their own notification)
                cx.emit(InputAreaEvent::StartedTyping);
                self.start_typing_monitor(cx);
            }
            TypingState::Composing(_) => {
                // Already typing - just update the timestamp (O(1), no allocations)
                // NO notification needed - just internal state update
                self.typing_state = TypingState::Composing(now);
            }
        }
    }

    /// Start the typing monitor task
    fn start_typing_monitor(&mut self, cx: &mut Context<Self>) {
        self.typing_monitor_task = None;

        self.typing_monitor_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            loop {
                crate::platform::sleep(TYPING_MONITOR_INTERVAL).await;

                let should_stop = entity
                    .update(cx, |view, cx| {
                        let TypingState::Composing(last_keystroke) = view.typing_state else {
                            return true;
                        };
                        if last_keystroke.elapsed() >= TYPING_PAUSED_TIMEOUT {
                            view.stop_typing_internal(cx);
                            return true;
                        }
                        false
                    })
                    .unwrap_or(true);

                if should_stop {
                    break;
                }
            }
        }));
    }

    fn stop_typing_internal(&mut self, cx: &mut Context<Self>) {
        if matches!(self.typing_state, TypingState::Composing(_)) {
            cx.emit(InputAreaEvent::StoppedTyping);
        }
        self.typing_state = TypingState::Idle;
        self.typing_monitor_task = None;
    }

    /// Reset typing state without emitting StoppedTyping (called by parent on
    /// chat switch, which routes the paused presence itself); otherwise the
    /// state machine would stay Composing and swallow the first keystroke's
    /// StartedTyping in the newly selected chat.
    /// The composer's focus target.
    ///
    /// Exposed as a handle rather than a `focus()` helper because focusing
    /// needs `&mut App`, which the caller already holds; returning the handle
    /// keeps this borrow read-only and lets the caller decide when to move
    /// focus.
    pub fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    pub fn reset_typing(&mut self) {
        self.typing_state = TypingState::Idle;
        self.typing_monitor_task = None;
    }

    /// Install the open group's mention candidates, and the addresses `@all`
    /// stands for. The roster can arrive after the chat opens, so this is
    /// called again on every update; only a different conversation resets
    /// what was already inserted.
    pub fn set_mentions(
        &mut self,
        members: Vec<MentionMember>,
        everyone: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.mention_members = members;
        self.mention_everyone = everyone;
        if self.mention_query.is_some() {
            self.refresh_mention_query(cx);
        }
        cx.notify();
    }

    /// Forget the picker and its inserted tokens, for a different recipient.
    pub fn reset_mentions(&mut self, cx: &mut Context<Self>) {
        self.mention_members.clear();
        self.mention_everyone.clear();
        self.inserted_mentions.clear();
        self.mention_query = None;
        self.mention_index = 0;
        self.mention_scroll
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        cx.notify();
    }

    /// Re-read the `@` token before the caret and open, update or close the
    /// picker accordingly.
    fn refresh_mention_query(&mut self, cx: &mut Context<Self>) {
        if self.mention_members.is_empty() {
            if self.mention_query.take().is_some() {
                cx.notify();
            }
            return;
        }
        let (text, cursor) = {
            let state = self.input.read(cx);
            (state.text().to_string(), state.cursor())
        };
        let Some(query) = mention_query_at(&text, cursor) else {
            self.close_mentions(cx);
            return;
        };
        let unchanged = self
            .mention_query
            .as_ref()
            .is_some_and(|open| open.start == query.start && open.text == query.text);
        if unchanged {
            return;
        }
        if self.mention_query.is_none() {
            log::info!("mention picker opened (query {:?})", query.text);
        }
        self.mention_query = Some(query);
        self.mention_index = 0;
        self.mention_scroll
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(0.0)));
        cx.notify();
    }

    fn close_mentions(&mut self, cx: &mut Context<Self>) {
        if self.mention_query.take().is_some() {
            self.mention_index = 0;
            cx.notify();
        }
    }

    /// The candidates left after the typed filter, in the order drawn.
    fn mention_results(&self) -> Vec<MentionMember> {
        let Some(query) = &self.mention_query else {
            return Vec::new();
        };
        let needle = query.text.trim().to_lowercase();
        if needle.is_empty() {
            return self.mention_members.clone();
        }
        self.mention_members
            .iter()
            .filter(|member| {
                member.label.to_lowercase().contains(&needle)
                    || member
                        .jid
                        .split('@')
                        .next()
                        .unwrap_or_default()
                        .contains(&needle)
            })
            .cloned()
            .collect()
    }

    /// Keep the highlighted row inside the picker's viewport.
    fn scroll_mention_into_view(&self) {
        let count = self.mention_results().len();
        if count == 0 {
            return;
        }
        self.mention_scroll
            .scroll_to_item(self.mention_index.min(count - 1));
    }

    /// Replace the typed `@token` with the highlighted member's name.
    fn accept_mention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(query) = self.mention_query.clone() else {
            return;
        };
        let results = self.mention_results();
        if results.is_empty() {
            return;
        }
        let candidate = results[self.mention_index.min(results.len() - 1)].clone();
        let range = {
            let text = self.input.read(cx).text().to_string();
            let cursor = self.input.read(cx).cursor().min(text.len());
            let start = text[..query.start].encode_utf16().count();
            let end = text[..cursor].encode_utf16().count();
            start..end.max(start)
        };
        let token = candidate.display_token();
        self.input.update(cx, |state, cx| {
            state.replace_text_in_range(Some(range), &token, window, cx);
        });
        self.inserted_mentions.push(candidate);
        self.mention_query = None;
        self.mention_index = 0;
        window.focus(&self.focus_handle(cx), cx);
        cx.notify();
    }

    /// What to send: the text as typed for the local bubble, and the wire's
    /// `@userpart` text plus the addresses it marks.
    fn mentions_for_send(&self, text: &str) -> (String, Vec<String>) {
        wire_mentions(text, &self.inserted_mentions, &self.mention_everyone)
    }

    /// What the composer currently holds, for the attachment caption.
    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).text().to_string()
    }

    /// Empty the composer after its text has been attached as a caption.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.inserted_mentions.clear();
        self.mention_query = None;
        self.forget_draft_link_preview();
        self.mention_index = 0;
    }

    /// Swap the composed text on chat switch: install the target chat's draft
    /// and hand the outgoing chat's draft back to the parent, so unsent text
    /// never rides along to a different recipient.
    pub fn swap_text(
        &mut self,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> String {
        let old = self.input.read(cx).text().to_string();
        self.input.update(cx, |state, cx| {
            state.set_value(new_text, window, cx);
        });
        self.inserted_mentions.clear();
        self.mention_query = None;
        self.mention_index = 0;
        self.forget_draft_link_preview();
        old
    }

    /// Read, trim-check, clear and emit the composed message (Enter and the
    /// send button share this path).
    fn submit_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_pending {
            return;
        }
        let text = self.input.read(cx).text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let (wire_text, mentions) = self.mentions_for_send(&text);
        let link_preview = self
            .draft_link_preview
            .clone()
            .filter(|preview| self.draft_link_url.as_deref() == Some(preview.url.as_str()));
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
        });
        self.inserted_mentions.clear();
        self.mention_query = None;
        self.mention_index = 0;
        self.forget_draft_link_preview();
        self.stop_typing_internal(cx);
        cx.emit(InputAreaEvent::SendMessage {
            text,
            wire_text,
            mentions,
            link_preview,
        });
    }

    /// Toggle PTT recording
    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        if self.is_recording {
            cx.emit(InputAreaEvent::StopRecording);
        } else {
            cx.emit(InputAreaEvent::StartRecording);
        }
    }

    /// Abandon the recording without sending it.
    fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        self.recording_started_at = None;
        self.level = 0.0;
        cx.emit(InputAreaEvent::CancelRecording);
    }

    /// Tell the composer the layout's slot height, so it stops guessing.
    pub fn set_layout(
        &mut self,
        height: gpui::Pixels,
        touch_target: gpui::Pixels,
        cx: &mut Context<Self>,
    ) {
        if self.height != Some(height) || self.touch_target != Some(touch_target) {
            self.height = Some(height);
            self.touch_target = Some(touch_target);
            cx.notify();
        }
    }

    pub fn set_recording(&mut self, is_recording: bool, cx: &mut Context<Self>) {
        if self.is_recording == is_recording {
            return;
        }
        self.is_recording = is_recording;
        self.recording_started_at = is_recording.then(Instant::now);
        if !is_recording {
            self.level = 0.0;
        }
        cx.notify();
    }

    /// The live input level, 0..=1.
    pub fn set_level(&mut self, level: f32, cx: &mut Context<Self>) {
        self.level = level.clamp(0.0, 1.0);
        cx.notify();
    }

    pub fn set_edit_state(&mut self, editing: bool, pending: bool, cx: &mut Context<Self>) {
        self.editing = editing;
        self.edit_pending = pending;
        cx.notify();
    }

    pub fn set_reply(&mut self, reply: Option<ReplyDraft>, cx: &mut Context<Self>) {
        self.reply = reply;
        cx.notify();
    }

    fn clear_reply(&mut self, cx: &mut Context<Self>) {
        self.reply = None;
        cx.emit(InputAreaEvent::CancelReply);
        cx.notify();
    }

    /// `m:ss` since recording started.
    fn recording_elapsed(&self) -> String {
        let secs = self
            .recording_started_at
            .map(|start| start.elapsed().as_secs())
            .unwrap_or(0);
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

impl Render for InputAreaView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let language = crate::i18n::Language::current();
        if self.language != language {
            self.language = language;
            self.input.update(cx, |input, cx| {
                input.set_placeholder(crate::tr!(TypeAMessage), window, cx)
            });
        }
        let is_recording = self.is_recording;
        let entity = cx.entity().clone();
        // The composer draws itself into the slot the layout gave it. Owning
        // a height constant of its own is what used to clip it by 6px on
        // mobile, where the layout said 56 and this said 62.
        let metrics = cx.product().metrics;
        let height = self.height.unwrap_or_else(|| metrics.composer_height());
        let control = self.touch_target.unwrap_or_else(|| metrics.icon_button());

        div()
            // A floor, not a ceiling: the layout's slot is what an empty
            // composer occupies, and the field grows the strip from there.
            .min_h(height)
            .flex_shrink_0()
            .relative()
            .flex()
            .flex_col()
            .justify_center()
            .py(metrics.space_sm())
            .px(metrics.space_xl())
            .bg(cx.theme().background)
            .border_t_1()
            .border_color(cx.theme().border)
            // The picker floats over the conversation instead of pushing it
            // up: the composer is anchored, and a list appearing above it
            // must not move the text the reader is looking at.
            .children(
                (!is_recording && self.mention_query.is_some())
                    .then(|| self.render_mentions(metrics, cx)),
            )
            .children(self.editing.then(|| {
                let entity = entity.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(if self.edit_pending {
                        crate::tr!(SavingEdit)
                    } else {
                        crate::tr!(EditingMessage)
                    })
                    .child(
                        Button::new("cancel-edit")
                            .ghost()
                            .label(crate::tr!(Cancel))
                            .disabled(self.edit_pending)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |_, cx| cx.emit(InputAreaEvent::CancelEdit));
                            }),
                    )
            }))
            .children(
                self.reply
                    .clone()
                    .filter(|_| !self.editing)
                    .map(|reply| render_reply_bar(reply, entity.clone(), metrics, cx)),
            )
            .children((self.emoji_open && !is_recording).then(|| self.render_picker(metrics, cx)))
            .child(if is_recording {
                self.render_recording(control, metrics, cx)
                    .into_any_element()
            } else {
                self.render_composer(control, metrics, cx)
                    .into_any_element()
            })
    }
}

impl InputAreaView {
    /// The `@` list: the group's people, filtered by what has been typed
    /// after the `@`. It floats above the composer, the way WhatsApp's does.
    fn render_mentions(
        &self,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let results = self.mention_results();
        let selected = self.mention_index.min(results.len().saturating_sub(1));
        // Shorter than a chat row: a picker is a menu, not a second sidebar.
        // Four rows and a scroll keep it from climbing the conversation; the
        // wheel still reaches everyone.
        let row_height = metrics.chat_row_height() * 0.62;
        div()
            .id("mention-picker")
            .debug_selector(|| "mention-picker".into())
            .absolute()
            .left(metrics.space_xl())
            .bottom(gpui::relative(1.0))
            .w(metrics.space_xxxl() * 10.0)
            .max_h(row_height * 4.2)
            .overflow_y_scroll()
            .track_scroll(&self.mention_scroll)
            // The picker owns the pixels it covers: a wheel or click over it
            // must not reach the conversation behind.
            .occlude()
            .rounded(metrics.radius_lg())
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_lg()
            .py(metrics.space_xs())
            .children(
                results
                    .into_iter()
                    .take(64)
                    .enumerate()
                    .map(|(index, member)| {
                        let selected_here = index == selected;
                        let jid = member.jid.clone();
                        let label = member.label.clone();
                        let picture = member.picture.clone();
                        let is_all = member.is_all;
                        let avatar = metrics.avatar_inline() * 0.9;
                        let ink = if selected_here {
                            cx.theme().primary_foreground
                        } else {
                            cx.theme().foreground
                        };
                        div()
                            .id(SharedString::from(format!("mention-{jid}")))
                            .flex()
                            .items_center()
                            .gap(metrics.space_xs())
                            .h(row_height)
                            .mx(metrics.space_xxs())
                            .px(metrics.space_xs())
                            .rounded(metrics.radius_md())
                            .when(selected_here, |el| el.bg(cx.theme().primary))
                            .cursor_pointer()
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.mention_index = index;
                                view.accept_mention(window, cx);
                            }))
                            .child(
                                Avatar::new(
                                    if is_all {
                                        "mention-all".to_string()
                                    } else {
                                        jid.clone()
                                    },
                                    if is_all { "@all" } else { label.as_str() },
                                    avatar,
                                )
                                .group(is_all)
                                .picture(picture, None)
                                .on(if selected_here {
                                    cx.theme().primary
                                } else {
                                    cx.theme().secondary
                                }),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics.text_body())
                                    .text_color(ink)
                                    .child(if is_all { "@all".to_string() } else { label }),
                            )
                    }),
            )
    }

    fn render_picker(&self, metrics: Metrics, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tab = self.picker_tab;
        let search = match tab {
            PickerTab::Emoji => &self.emoji_search,
            PickerTab::Gif => &self.gif_search,
        };
        div()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .pb(metrics.space_md())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.space_sm())
                    .child(div().flex_1().child(Input::new(search)))
                    .child(
                        Button::new("picker-close")
                            .icon(IconName::Close)
                            .ghost()
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.emoji_open = false;
                                window.focus(&view.focus_handle(cx), cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.space_xs())
                    .child(
                        Button::new("picker-tab-emoji")
                            .label("Emoji")
                            .small()
                            .when(tab == PickerTab::Emoji, |button| button.primary())
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.picker_tab = PickerTab::Emoji;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("picker-tab-gif")
                            .label("GIFs")
                            .small()
                            .when(tab == PickerTab::Gif, |button| button.primary())
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.picker_tab = PickerTab::Gif;
                                cx.notify();
                            })),
                    ),
            )
            .child(match tab {
                PickerTab::Emoji => self.render_emoji_grid(metrics, cx).into_any_element(),
                PickerTab::Gif => self.render_gif_grid(metrics, cx).into_any_element(),
            })
    }

    fn render_emoji_grid(
        &self,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        #[derive(serde::Deserialize)]
        struct Emoji {
            emoji: String,
            name: String,
        }
        static EMOJI: std::sync::OnceLock<Vec<Emoji>> = std::sync::OnceLock::new();
        let entries = EMOJI.get_or_init(|| {
            serde_json::from_str(include_str!("../../assets/emoji.json"))
                .expect("bundled Unicode emoji data")
        });
        let query = self.emoji_search.read(cx).value().to_lowercase();
        let matches: Vec<_> = entries
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.name.to_lowercase().contains(&query)
                    || item.emoji.contains(&query)
            })
            .take(120)
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .child(
                div()
                    .id("emoji-grid")
                    .h(metrics.touch_target() * 3.0)
                    .overflow_y_scroll()
                    .flex()
                    .flex_wrap()
                    .gap(metrics.space_xs())
                    .children(matches.into_iter().enumerate().map(|(index, item)| {
                        let emoji = item.emoji.clone();
                        Button::new(("emoji-pick", index))
                            .label(item.emoji.clone())
                            .tooltip(item.name.clone())
                            .ghost()
                            .size(metrics.touch_target())
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.input.update(cx, |state, cx| {
                                    state.replace_text_in_range(None, &emoji, window, cx)
                                });
                                window.focus(&view.focus_handle(cx), cx);
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .text_size(metrics.text_small())
                    .text_color(cx.theme().muted_foreground)
                    .child("Search to find more emoji, including skin tones and flags."),
            )
    }

    fn render_gif_grid(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::AnyElement {
        if crate::platform::gif::api_key().is_none() {
            return div()
                .flex()
                .flex_col()
                .gap(metrics.space_sm())
                .child(
                    div()
                        .text_size(metrics.text_small())
                        .child("GIF search needs a KLIPY API key."),
                )
                .child(
                    div()
                        .text_size(metrics.text_micro())
                        .text_color(cx.theme().muted_foreground)
                        .child("Set WASP_KLIPY_KEY to the key from klipy.com/developers."),
                )
                .child(
                    Button::new("gif-get-key")
                        .label("Get a key")
                        .small()
                        .on_click(|_, _, cx| cx.open_url("https://klipy.com/developers")),
                )
                .into_any_element();
        }

        let tile = metrics.touch_target() * 2.4;
        div()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .children(self.gif_error.as_ref().map(|error| {
                div()
                    .text_size(metrics.text_small())
                    .text_color(cx.theme().danger)
                    .child(error.clone())
            }))
            .when(self.gif_loading, |el| {
                el.child(
                    div()
                        .text_size(metrics.text_small())
                        .text_color(cx.theme().muted_foreground)
                        .child("Searching KLIPY…"),
                )
            })
            .child(
                div()
                    .id("gif-grid")
                    .h(metrics.touch_target() * 4.0)
                    .overflow_y_scroll()
                    .flex()
                    .flex_wrap()
                    .gap(metrics.space_xs())
                    .children(self.gif_results.iter().take(24).enumerate().map(
                        |(index, preview)| {
                            let source = preview
                                .image
                                .clone()
                                .map(|image| image.source(preview.animated));
                            div()
                                .id(("gif-pick", index))
                                .w(tile)
                                .h(tile)
                                .flex_shrink_0()
                                .overflow_hidden()
                                .rounded(metrics.radius_sm())
                                .bg(cx.theme().secondary)
                                .cursor_pointer()
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.choose_gif(index, cx);
                                }))
                                .map(|el| match source {
                                    Some(source) => el.child(
                                        gpui::img(source)
                                            .size_full()
                                            .object_fit(gpui::ObjectFit::Contain),
                                    ),
                                    None => el.child(
                                        div()
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(metrics.text_micro())
                                            .text_color(cx.theme().muted_foreground)
                                            .child(preview.result.title.clone()),
                                    ),
                                })
                        },
                    )),
            )
            .when(
                !self.gif_loading && self.gif_results.is_empty() && self.gif_error.is_none(),
                |el| {
                    el.child(
                        div()
                            .text_size(metrics.text_small())
                            .text_color(cx.theme().muted_foreground)
                            .child("Search KLIPY for a GIF."),
                    )
                },
            )
            .child(
                div()
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().muted_foreground)
                    .child("Powered by KLIPY"),
            )
            .into_any_element()
    }

    /// Open or close the emoji panel, for the window-wide shortcut.
    pub fn toggle_emoji(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_recording {
            return;
        }
        self.emoji_open = !self.emoji_open;
        self.picker_tab = PickerTab::Emoji;
        if self.emoji_open {
            self.emoji_search
                .update(cx, |state, cx| state.focus(window, cx));
        } else {
            window.focus(&self.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Open the GIF half of the picker, for `Ctrl+Shift+G`.
    pub fn open_gif_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_recording {
            return;
        }
        self.emoji_open = true;
        self.picker_tab = PickerTab::Gif;
        self.gif_search
            .update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Wait for a pause before reading the address, so the document is copied
    /// once per pause rather than once per keystroke.
    fn queue_draft_link_probe(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        self.link_probe_task = Some(cx.spawn(async move |_, cx| {
            crate::platform::sleep(Duration::from_millis(500)).await;
            let _ = entity.update(cx, |view, cx| view.refresh_draft_link_preview(cx));
        }));
    }

    /// Read the first address in the text and replace the composer's card.
    fn refresh_draft_link_preview(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_string();
        let url = crate::platform::link_preview::first_url(&text);
        if url == self.draft_link_url {
            return;
        }
        self.forget_draft_link_preview();
        self.draft_link_url = url.clone();
        self.draft_link_pending = url.is_some();
        let Some(url) = url else {
            cx.notify();
            return;
        };
        self.fetch_draft_link_preview(url, cx);
        cx.notify();
    }

    fn fetch_draft_link_preview(&mut self, url: String, cx: &mut Context<Self>) {
        let task = crate::platform::link_preview::fetch(cx, url.clone());
        let entity = cx.entity().downgrade();
        self.link_fetch_task = Some(cx.spawn(async move |_, cx| {
            let result = crate::platform::with_timeout(task, Duration::from_secs(8)).await;
            let _ = entity.update(cx, |view, cx| {
                if view.draft_link_url.as_deref() != Some(url.as_str()) {
                    return;
                }
                view.draft_link_pending = false;
                view.draft_link_preview = match result {
                    Some(Ok(preview)) => Some(preview),
                    Some(Err(error)) => {
                        log::debug!("composer link preview: {url} failed: {error}");
                        None
                    }
                    None => {
                        log::debug!("composer link preview: {url} timed out");
                        None
                    }
                };
                cx.notify();
            });
        }));
    }

    fn forget_draft_link_preview(&mut self) {
        self.draft_link_url = None;
        self.draft_link_preview = None;
        self.draft_link_pending = false;
        self.link_probe_task = None;
        self.link_fetch_task = None;
    }

    fn queue_gif_search(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        self.gif_task = Some(cx.spawn(async move |_, cx| {
            crate::platform::sleep(Duration::from_millis(220)).await;
            let _ = entity.update(cx, |view, cx| view.search_gifs(cx));
        }));
    }

    fn search_gifs(&mut self, cx: &mut Context<Self>) {
        let query = self.gif_search.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.gif_results.clear();
            self.gif_loading = false;
            self.gif_error = None;
            cx.notify();
            return;
        }
        self.gif_loading = true;
        self.gif_error = None;
        let task = crate::platform::gif::search(cx, query, "velo".to_string());
        self.gif_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                view.gif_loading = false;
                match result {
                    Ok(results) => {
                        view.gif_results = results
                            .into_iter()
                            .map(|result| GifPreview {
                                result,
                                image: None,
                                animated: false,
                            })
                            .collect();
                        view.fetch_gif_previews(cx);
                    }
                    Err(error) => {
                        view.gif_results.clear();
                        view.gif_error = Some(error);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Fetch the small animated previews, newest query first and only the
    /// tiles the panel can show. Four at a time would be better than this
    /// one-per-tile start, but each completion repaints the grid as it lands.
    fn fetch_gif_previews(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<(usize, String)> = self
            .gif_results
            .iter()
            .take(12)
            .enumerate()
            .filter_map(|(index, preview)| {
                preview.result.preview_url.clone().map(|url| (index, url))
            })
            .collect();
        for (index, url) in urls {
            let task = crate::platform::gif::fetch(cx, url);
            cx.spawn(async move |this: WeakEntity<Self>, cx| {
                let Ok(bytes) = task.await else {
                    return;
                };
                let _ = this.update(cx, |view, cx| {
                    let Some(preview) = view.gif_results.get_mut(index) else {
                        return;
                    };
                    let Some(mime) = crate::platform::picker::previewable_image_mime(&bytes) else {
                        return;
                    };
                    let Some(format) = gpui::ImageFormat::from_mime_type(mime) else {
                        return;
                    };
                    preview.animated = matches!(mime, "image/webp" | "image/gif");
                    preview.image = Some(crate::retained_image::RetainedImage::new(
                        gpui::Image::from_bytes(format, bytes.as_ref().clone()),
                    ));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn choose_gif(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(preview) = self.gif_results.get(index) else {
            return;
        };
        let Some(url) = preview.result.video_url.clone() else {
            return;
        };
        let task = crate::platform::gif::fetch(cx, url);
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let Ok(bytes) = task.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                view.emoji_open = false;
                cx.emit(InputAreaEvent::SendGif { bytes });
                cx.notify();
            });
        })
        .detach();
    }

    /// The ordinary state: attach, field, emoji, and either record or send.
    fn render_composer(
        &self,
        control: gpui::Pixels,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let entity = cx.entity().clone();
        let record_entity = entity.clone();
        let attach_entity = entity.clone();
        // Asked of the rope rather than of a copy of it. `text()` hands back
        // the document itself; `to_string` copied all of it, on every
        // keystroke, to answer whether the send button or the microphone
        // belongs here.
        let has_text = self
            .input
            .read(cx)
            .text()
            .chars()
            .any(|c| !c.is_whitespace());

        div()
            .key_context("MessageComposer")
            .on_action(|action: &Enter, _, cx| {
                if !action.shift {
                    // The textarea propagates submission. Consume it here so
                    // GPUI cannot also insert the key's newline as text input.
                    cx.stop_propagation();
                }
            })
            .flex()
            .items_center()
            .gap(metrics.space_md())
            .child(
                parts::icon_button(
                    "attach",
                    Icon::new(ProductIcon::Paperclip),
                    crate::tr!(AttachAPhotoAVideoOrADocument),
                    control,
                )
                .disabled(self.editing)
                .cursor_pointer()
                .debug_selector(|| "attach-trigger".into())
                .dropdown_menu(move |menu, _window, _cx| {
                    let sticker_entity = attach_entity.clone();
                    let gif_entity = attach_entity.clone();
                    let photos_entity = attach_entity.clone();
                    let document_entity = attach_entity.clone();
                    // Document and photos first: they are what an attachment
                    // usually is, and the keyboard lands on the first item.
                    menu.item(
                        PopupMenuItem::new(crate::tr!(Document))
                            .icon(Icon::from(ProductIcon::FileText))
                            .on_click(move |_, _window, cx| {
                                document_entity.update(cx, |_view, cx| {
                                    cx.emit(InputAreaEvent::AttachFiles(
                                        AttachmentCategory::Document,
                                    ));
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(crate::tr!(PhotosAndVideos))
                            .icon(Icon::from(ProductIcon::Image))
                            .on_click(move |_, _window, cx| {
                                photos_entity.update(cx, |_view, cx| {
                                    cx.emit(InputAreaEvent::AttachFiles(
                                        AttachmentCategory::PhotosVideos,
                                    ));
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("GIF or looping video")
                            .icon(Icon::from(ProductIcon::Film))
                            .on_click(move |_, _, cx| {
                                gif_entity.update(cx, |_, cx| {
                                    cx.emit(InputAreaEvent::AttachFiles(AttachmentCategory::Gif))
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Sticker from image")
                            .icon(Icon::from(ProductIcon::Sticker))
                            .on_click(move |_, _, cx| {
                                sticker_entity.update(cx, |_, cx| {
                                    cx.emit(InputAreaEvent::AttachFiles(
                                        AttachmentCategory::Sticker,
                                    ))
                                });
                            }),
                    )
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .capture_action::<Paste>(cx.listener(|view, _: &Paste, _window, cx| {
                        // Only consume the key when there is media to send.
                        // A text clipboard still belongs to the textarea, and
                        // a copied picture must not leave its path behind.
                        if crate::platform::clipboard::holds_media(cx) {
                            view.paste_image(cx);
                            cx.stop_propagation();
                        }
                    }))
                    // A soft pill with no outline, the way the desktop
                    // messengers draw their composer: the caret says where
                    // focus is; a coloured ring around the whole field
                    // shouted it on every keystroke.
                    .rounded(metrics.radius_xl())
                    .bg(cx.theme().secondary)
                    .px(metrics.space_md())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(metrics.space_xxs())
                            .w_full()
                            .children(
                                self.draft_link_preview
                                    .as_ref()
                                    .map(|preview| render_draft_link_preview(preview, metrics, cx)),
                            )
                            .when(
                                self.draft_link_pending && self.draft_link_preview.is_none(),
                                |column| column.child(render_draft_link_skeleton(metrics, cx)),
                            )
                            .child(
                                Textarea::new(&self.input)
                                    .readonly(self.edit_pending)
                                    .bordered(false)
                                    .w_full(),
                            ),
                    ),
            )
            .child(
                parts::icon_button(
                    "emoji",
                    Icon::new(ProductIcon::Smile),
                    "Emoji, GIFs and stickers",
                    control,
                )
                .disabled(self.edit_pending)
                .on_click(cx.listener(|view, _, _, cx| {
                    view.emoji_open = !view.emoji_open;
                    cx.notify();
                })),
            )
            // Record or send, never both: which one is available follows
            // whether there is anything to send, the way every messaging
            // client behaves.
            .child(if has_text || self.editing {
                Button::new("send")
                    .icon(IconName::ArrowRight)
                    .primary()
                    .tooltip(if self.editing {
                        crate::tr!(SaveEdit)
                    } else {
                        crate::tr!(Send)
                    })
                    .disabled(self.edit_pending || (self.editing && !has_text))
                    .w(control)
                    .h(control)
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |view, cx| view.submit_input(window, cx));
                    })
                    .map(|button| crate::motion::swap_in(button, "composer-send-in"))
                    .into_any_element()
            } else {
                // Drawn disabled where nothing can come of pressing it, with
                // the reason in the tooltip. A control that looks live and
                // does nothing is the worse answer: the browser has no Opus
                // encoder, and that is knowable before the microphone is ever
                // asked for.
                let can_record = self.can_record;
                Button::new("ptt")
                    .icon(ProductIcon::Mic)
                    .ghost()
                    .disabled(!can_record || self.editing)
                    .tooltip(if can_record {
                        crate::tr!(HoldToRecordAVoiceMessage)
                    } else {
                        crate::tr!(VoiceMessagesCannotBeRecordedInThisBrowser)
                    })
                    .w(control)
                    .h(control)
                    .when(can_record, |button| {
                        button.cursor_pointer().on_click(move |_, _window, cx| {
                            record_entity.update(cx, |view, cx| view.toggle_recording(cx));
                        })
                    })
                    .into_any_element()
            })
    }

    /// Recording: elapsed time, a live level, and the two ways out.
    ///
    /// The old UI turned the microphone button red and said nothing else — no
    /// duration, no level, and no way to abandon a recording without sending
    /// it.
    fn render_recording(
        &self,
        control: gpui::Pixels,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let entity = cx.entity().clone();
        let cancel_entity = entity.clone();

        div()
            .flex()
            .items_center()
            .gap(metrics.space_lg())
            .child(
                parts::icon_button(
                    "cancel-recording",
                    Icon::new(ProductIcon::Trash),
                    crate::tr!(DiscardRecording),
                    control,
                )
                .cursor_pointer()
                .on_click(move |_, _window, cx| {
                    cancel_entity.update(cx, |view, cx| view.cancel_recording(cx));
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.space_md())
                    .child(
                        div()
                            .size(metrics.space_md())
                            .rounded_full()
                            .bg(cx.theme().danger),
                    )
                    .child(
                        div()
                            .font_features(crate::components::parts::tabular())
                            .text_size(metrics.text_meta())
                            .text_color(cx.theme().foreground)
                            .child(self.recording_elapsed()),
                    ),
            )
            .child(render_level(self.level, metrics, cx))
            .child(
                div()
                    .text_size(metrics.text_small())
                    .text_color(parts::subtle(cx))
                    .child(crate::tr!(Recording)),
            )
            .child(
                Button::new("send-recording")
                    .icon(IconName::ArrowRight)
                    .primary()
                    .tooltip(crate::tr!(SendVoiceMessage))
                    .w(control)
                    .h(control)
                    .cursor_pointer()
                    .on_click(move |_, _window, cx| {
                        entity.update(cx, |view, cx| view.toggle_recording(cx));
                    }),
            )
    }
}

/// The live input level while recording.
fn render_level(level: f32, metrics: Metrics, cx: &App) -> impl IntoElement + use<> {
    const BARS: usize = 16;
    let full = metrics.waveform_height();
    let level = level.clamp(0.0, 1.0);

    div()
        .flex_1()
        .h(full)
        .flex()
        .items_center()
        .gap(metrics.waveform_bar_gap())
        .children((0..BARS).map(move |ix| {
            // A fixed envelope scaled by the live level, so the bars move with
            // the voice without flickering randomly between frames.
            let position = (ix as f32 + 0.5) / BARS as f32;
            let envelope = 0.35 + 0.65 * (position * std::f32::consts::PI).sin();
            div()
                .w(metrics.waveform_bar_width())
                .h((full * envelope * level).max(metrics.bar_thin()))
                .rounded_full()
                .bg(cx.theme().primary)
        }))
}

/// The message being replied to, above the field, with a way to drop it.
fn render_reply_bar(
    reply: ReplyDraft,
    entity: Entity<InputAreaView>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let hue = cx.product().speaker(&reply.sender);
    let replying_to = crate::tr!(
        replying_to,
        reply.sender_label(crate::i18n::Language::current())
    );
    let summary = if reply.preview.is_empty() {
        reply
            .kind
            .map(|kind| crate::i18n::Language::current().quoted_kind(kind))
            .unwrap_or_default()
            .to_owned()
    } else {
        reply.preview
    };

    div()
        .flex()
        .items_center()
        .gap(metrics.space_md())
        .pb(metrics.space_md())
        .child(
            div()
                .w(metrics.selection_bar_width())
                .h(metrics.avatar_inline())
                .rounded_full()
                .flex_shrink_0()
                .bg(hue),
        )
        .child(
            parts::detail_stack()
                .child(
                    div()
                        .text_size(metrics.text_small())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(hue)
                        .child(replying_to),
                )
                .child(
                    parts::one_line()
                        .text_size(metrics.text_secondary())
                        .text_color(cx.theme().muted_foreground)
                        .child(summary),
                ),
        )
        .child(
            Button::new("cancel-reply")
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .tooltip(crate::tr!(CancelReply))
                .cursor_pointer()
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |view, cx| view.clear_reply(cx));
                }),
        )
}

/// The card read for the address currently being composed.
///
/// A line rather than a picture: the composer only needs to say that the
/// metadata is there, and decoding a thumbnail in the field would spend more
/// than the message itself.
fn render_draft_link_preview(
    preview: &velo_core::LinkPreview,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let host = SharedString::from(preview.host().to_string());
    let site = SharedString::from(
        preview
            .site_name
            .clone()
            .filter(|site| !site.trim().is_empty())
            .unwrap_or_else(|| host.to_string()),
    );
    let title = SharedString::from(
        preview
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| site.to_string()),
    );
    div()
        .id("draft-link-preview")
        .debug_selector(|| "draft-link-preview".into())
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(metrics.space_xs())
        .px(metrics.space_xs())
        .py(metrics.space_xxs())
        .rounded(metrics.radius_sm())
        .bg(cx.theme().background)
        .child(
            Icon::new(IconName::ExternalLink)
                .size(metrics.icon_small())
                .text_color(cx.theme().primary),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(metrics.text_micro())
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(site),
                )
                .child(
                    div()
                        .text_size(metrics.text_small())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .truncate()
                        .child(title),
                ),
        )
}

fn render_draft_link_skeleton(metrics: Metrics, cx: &App) -> impl IntoElement + use<> {
    div()
        .id("draft-link-preview-loading")
        .debug_selector(|| "draft-link-preview-loading".into())
        .w_full()
        .flex()
        .items_center()
        .gap(metrics.space_xs())
        .px(metrics.space_xs())
        .py(metrics.space_xxs())
        .rounded(metrics.radius_sm())
        .bg(cx.theme().background)
        .child(
            Icon::new(IconName::LoaderCircle)
                .size(metrics.icon_small())
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .text_size(metrics.text_micro())
                .text_color(cx.theme().muted_foreground)
                .child(crate::tr!(ReadingLinkPreview)),
        )
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use gpui::{
        AppContext as _, ClipboardItem, Entity, Image, ImageFormat, IntoElement, Keystroke,
        ParentElement, Render, Styled as _, Subscription, Window, div, px, size,
    };

    use super::{InputAreaEvent, InputAreaView, ReplyDraft};
    use crate::platform::picker::AttachmentCategory;

    struct ComposerHarness {
        input: Entity<InputAreaView>,
    }

    struct ComposerFixture {
        cx: gpui::HeadlessAppContext,
        window: gpui::WindowHandle<ComposerHarness>,
        input: Entity<InputAreaView>,
        paste_requests: Rc<RefCell<usize>>,
        _events: Subscription,
    }

    impl ComposerHarness {
        fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
            let input = cx.new(|cx| InputAreaView::new(window, cx));
            Self { input }
        }
    }

    impl Render for ComposerHarness {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div().size_full().child(self.input.clone())
        }
    }

    #[test]
    fn reply_drafts_keep_self_identity_separate_from_contact_names() {
        use crate::i18n::Language;
        let mut draft = ReplyDraft {
            message_id: "synthetic".into(),
            sender: "1@s.whatsapp.net".into(),
            sender_name: "Account owner".into(),
            sender_is_self: true,
            preview: String::new(),
            kind: None,
        };
        assert_eq!(draft.sender_label(Language::Portuguese), "Você");
        assert_eq!(draft.sender_label(Language::English), "You");
        let quote: velo_core::QuotedMessage = draft.clone().into();
        assert_eq!(quote.sender_name, "Account owner");
        draft.sender_is_self = false;
        draft.sender_name = "Você".into();
        assert_eq!(draft.sender_label(Language::English), "Você");
    }

    fn setup() -> ComposerFixture {
        let mut cx = gpui::HeadlessAppContext::with_asset_source(
            Arc::new(gpui_wgpu::CosmicTextSystem::new("DejaVu Sans")),
            Arc::new(crate::assets::Assets),
        );
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::app::init_app_bindings(cx);
        });
        let window = cx
            .open_window(size(px(640.), px(120.)), |window, cx| {
                cx.new(|cx| ComposerHarness::new(window, cx))
            })
            .unwrap();
        cx.run_until_parked();
        let input = window
            .update(&mut cx, |harness, _, _| harness.input.clone())
            .unwrap();
        let paste_requests = Rc::new(RefCell::new(0));
        let observed = paste_requests.clone();
        let events = cx.update(|cx| {
            cx.subscribe(&input, move |_, event: &InputAreaEvent, _| {
                if matches!(event, InputAreaEvent::PasteMedia) {
                    *observed.borrow_mut() += 1;
                }
            })
        });
        cx.update_window(*window, |_, window, cx| {
            input.read(cx).focus_handle(cx).focus(window, cx);
        })
        .unwrap();
        cx.run_until_parked();
        ComposerFixture {
            cx,
            window,
            input,
            paste_requests,
            _events: events,
        }
    }

    fn paste_shortcut() -> Keystroke {
        Keystroke::parse(if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        })
        .unwrap()
    }

    fn paste(cx: &mut gpui::HeadlessAppContext, window: &gpui::WindowHandle<ComposerHarness>) {
        cx.update_window((*window).into(), |_, window, cx| {
            window.dispatch_keystroke(paste_shortcut(), cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    struct VisualComposerFixture {
        cx: gpui::VisualTestContext,
        input: Entity<InputAreaView>,
        attachment_categories: Rc<RefCell<Vec<AttachmentCategory>>>,
        _events: Subscription,
    }

    fn visual_setup(cx: &mut gpui::TestAppContext) -> VisualComposerFixture {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let mut input_entity = None;
        let window = cx.open_window(size(px(640.), px(120.)), |window, cx| {
            let input = cx.new(|cx| InputAreaView::new(window, cx));
            input_entity = Some(input.clone());
            ComposerHarness { input }
        });
        let input = input_entity.expect("composer input should be created");
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let attachment_categories = Rc::new(RefCell::new(Vec::new()));
        let observed = attachment_categories.clone();
        let events = cx.update(|_window, cx| {
            cx.subscribe(&input, move |_, event: &InputAreaEvent, _| {
                if let InputAreaEvent::AttachFiles(category) = event {
                    observed.borrow_mut().push(*category);
                }
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        VisualComposerFixture {
            cx,
            input,
            attachment_categories,
            _events: events,
        }
    }

    #[test]
    fn modified_enter_inserts_newlines_without_sending_and_plain_enter_sends_once() {
        let ComposerFixture {
            mut cx,
            window,
            input,
            ..
        } = setup();
        let sent = Rc::new(RefCell::new(Vec::new()));
        let observed = sent.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&input, move |_, event: &InputAreaEvent, _| {
                if let InputAreaEvent::SendMessage { text, .. } = event {
                    observed.borrow_mut().push(text.clone());
                }
            })
        });
        cx.update_window(*window, |_, window, cx| {
            input.update(cx, |view, cx| {
                view.input.update(cx, |field, cx| {
                    field.set_value("hello", window, cx);
                    field.set_cursor_position(
                        gpui_component::input::Position::new(0, 5),
                        window,
                        cx,
                    );
                })
            });
        })
        .unwrap();
        for key in ["ctrl-enter", "shift-enter"] {
            cx.update_window(*window, |_, window, cx| {
                window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx);
            })
            .unwrap();
            cx.run_until_parked();
            assert!(sent.borrow().is_empty(), "{key} submitted the message");
        }
        cx.update(|cx| {
            assert_eq!(
                input.read(cx).input.read(cx).text().to_string(),
                "hello\n\n"
            )
        });
        cx.update_window(*window, |_, window, cx| {
            window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(&*sent.borrow(), &["hello\n\n"]);
        cx.update(|cx| assert!(input.read(cx).input.read(cx).text().to_string().is_empty()));
    }

    #[test]
    fn secondary_confirmation_still_submits_without_a_newline() {
        let ComposerFixture {
            mut cx,
            window,
            input,
            ..
        } = setup();
        let sent = Rc::new(RefCell::new(Vec::new()));
        let observed = sent.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&input, move |_, event: &InputAreaEvent, _| {
                if let InputAreaEvent::SendMessage { text, .. } = event {
                    observed.borrow_mut().push(text.clone());
                }
            })
        });
        cx.update_window(*window, |_, window, cx| {
            input.update(cx, |view, cx| {
                view.swap_text("command send", window, cx);
            });
            if cfg!(target_os = "macos") {
                window.dispatch_keystroke(Keystroke::parse("cmd-enter").unwrap(), cx);
            } else {
                window.dispatch_action(
                    Box::new(gpui_component::input::Enter {
                        secondary: true,
                        shift: false,
                    }),
                    cx,
                );
            }
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(&*sent.borrow(), &["command send"]);
    }

    #[test]
    fn focused_composer_requests_a_clipboard_media_read() {
        let ComposerFixture {
            mut cx,
            window,
            paste_requests,
            _events,
            ..
        } = setup();
        let bytes = vec![1, 2, 3, 4];
        cx.update(|cx| {
            cx.write_to_clipboard(ClipboardItem::new_image(&Image {
                format: ImageFormat::Png,
                bytes: bytes.clone(),
                id: 0,
            }));
        });

        paste(&mut cx, &window);

        // The full app fixture checks the actual clipboard bytes. This
        // isolated composer requests the parent to read only after it has
        // reserved a slot, never reading paths here before that decision.
        assert_eq!(*paste_requests.borrow(), 1);
    }

    #[test]
    fn focused_composer_still_pastes_text_once() {
        let ComposerFixture {
            mut cx,
            window,
            input,
            paste_requests,
            _events,
            ..
        } = setup();
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("hello".into())));

        paste(&mut cx, &window);

        cx.update(|cx| assert_eq!(input.read(cx).input.read(cx).text(), "hello"));
        assert_eq!(
            *paste_requests.borrow(),
            0,
            "a text clipboard belongs to the textarea, not to the media path"
        );
    }

    #[gpui::test]
    fn typing_at_offers_the_group_and_enter_inserts_a_name(cx: &mut gpui::TestAppContext) {
        let VisualComposerFixture { mut cx, input, .. } = visual_setup(cx);
        let everyone = vec!["559900000001@s.whatsapp.net".to_string()];
        cx.update(|window, cx| {
            input.update(cx, |view, cx| {
                view.set_mentions(
                    vec![
                        mention("Ana", "559900000001@s.whatsapp.net"),
                        mention("Bia", "559900000002@s.whatsapp.net"),
                    ],
                    everyone.clone(),
                    cx,
                );
                view.input.update(cx, |field, cx| {
                    field.set_value("hola @a", window, cx);
                    field.set_cursor_position(
                        gpui_component::input::Position::new(0, 7),
                        window,
                        cx,
                    );
                });
                view.refresh_mention_query(cx);
                view.focus_handle(cx).focus(window, cx);
            });
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("mention-picker").is_some(),
            "typing @ in a group should open the picker"
        );
        cx.simulate_keystrokes("down enter");
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_eq!(
                input.read(cx).input.read(cx).text().to_string(),
                "hola @Bia "
            );
        });
        assert!(
            cx.debug_bounds("mention-picker").is_none(),
            "the picker closes once a name is inserted"
        );
    }

    #[gpui::test]
    fn attachment_menu_stays_silent_until_a_category_is_confirmed(
        test_cx: &mut gpui::TestAppContext,
    ) {
        let VisualComposerFixture {
            mut cx,
            attachment_categories,
            _events,
            ..
        } = visual_setup(test_cx);

        let attach = cx
            .debug_bounds("attach-trigger")
            .expect("attachment button should be rendered");
        cx.simulate_click(attach.center(), gpui::Modifiers::default());
        cx.run_until_parked();

        assert!(
            attachment_categories.borrow().is_empty(),
            "opening the category menu must not emit an attachment event"
        );

        // The menu is keyboard-focused on open. Its first item is Document,
        // matching the user-facing menu order and preserving keyboard access.
        // The pointer is moved off the menu first: a taller menu opens under
        // the click, and hovering an item selects it, which is a fact about
        // the mouse rather than the keyboard order this test is about.
        let away = gpui::point(gpui::px(1.0), gpui::px(1.0));
        cx.simulate_mouse_move(away, None, gpui::Modifiers::default());
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        assert_eq!(
            attachment_categories.borrow().as_slice(),
            [AttachmentCategory::Document]
        );

        cx.simulate_click(attach.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        cx.simulate_mouse_move(away, None, gpui::Modifiers::default());
        cx.simulate_keystrokes("down down enter");
        cx.run_until_parked();
        assert_eq!(
            attachment_categories.borrow().as_slice(),
            [
                AttachmentCategory::Document,
                AttachmentCategory::PhotosVideos
            ]
        );
    }

    fn mention(label: &str, jid: &str) -> super::MentionMember {
        super::MentionMember {
            jid: jid.to_string(),
            label: label.to_string(),
            picture: None,
            is_all: false,
        }
    }

    #[test]
    fn an_at_token_opens_only_where_a_word_starts() {
        assert_eq!(
            super::mention_query_at("hola @an", 8).map(|q| (q.start, q.text)),
            Some((5, "an".to_string()))
        );
        assert_eq!(
            super::mention_query_at("@", 1).map(|q| q.text),
            Some(String::new())
        );
        assert!(super::mention_query_at("mail a@b", 8).is_none());
        assert!(super::mention_query_at("@ana done", 9).is_none());
        assert!(super::mention_query_at("no at here", 10).is_none());
    }

    #[test]
    fn mention_labels_leave_as_addresses_and_all_names_everyone() {
        let ana = mention("Ana", "559900000001:4@s.whatsapp.net");
        let ana_maria = mention("Ana Maria", "559900000002@s.whatsapp.net");
        let everybody = super::MentionMember {
            jid: String::new(),
            label: "all".to_string(),
            picture: None,
            is_all: true,
        };
        let (wire, jids) = super::wire_mentions(
            "hola @Ana Maria y @Ana, @all",
            &[ana, ana_maria, everybody],
            &["559900000001@s.whatsapp.net".to_string()],
        );
        assert_eq!(wire, "hola @559900000002 y @559900000001, @all");
        assert_eq!(
            jids,
            [
                "559900000002@s.whatsapp.net".to_string(),
                "559900000001:4@s.whatsapp.net".to_string(),
                "559900000001@s.whatsapp.net".to_string(),
            ]
        );
    }

    #[test]
    fn a_deleted_mention_is_not_sent() {
        let ana = mention("Ana", "559900000001@s.whatsapp.net");
        let (wire, jids) = super::wire_mentions("hola", &[ana], &[]);
        assert_eq!(wire, "hola");
        assert!(jids.is_empty());
    }
}
