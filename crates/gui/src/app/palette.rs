//! The floating command bar: chats, message hits and a few commands.
//!
//! `Ctrl+K` used to put the caret in the sidebar's filter. That filter can
//! only narrow the list that is already on screen; this surface is for the
//! question the shortcut actually asks — "where is that chat?" — while the
//! conversation stays where it is. The sidebar field is still there for
//! filtering, and it still keeps its own query.

use gpui::{AppContext as _, Context, Entity, Task, WeakEntity, Window};
use gpui_component::input::{InputEvent, InputState};

use super::{ChatOpen, WhatsAppApp};
use crate::app::global_search::GlobalHit;

/// What one row does when accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaletteAction {
    NewChat,
    Settings,
    Calls,
    Files,
    ConversationSearch,
}

/// A section the row belongs to, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PaletteKind {
    Chat,
    Message,
    Action,
}

impl PaletteKind {
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Self::Chat => "CHATS",
            Self::Message => "MESSAGES",
            Self::Action => "ACTIONS",
        }
    }
}

/// One selectable row. The fields a row does not use stay empty rather than
/// becoming a second enum: the renderer draws one shape with an optional
/// second line, and accepting one asks the kind what it means.
#[derive(Clone)]
pub(crate) struct PaletteRow {
    pub kind: PaletteKind,
    pub title: String,
    pub subtitle: String,
    pub jid: Option<String>,
    pub message_id: Option<String>,
    pub action: Option<PaletteAction>,
    pub avatar_key: Option<String>,
    pub unread: u32,
}

#[derive(Default)]
pub(crate) struct Palette {
    pub open: bool,
    pub query: String,
    pub rows: Vec<PaletteRow>,
    pub selected: usize,
    pub input: Option<Entity<InputState>>,
    /// Message hits for the current query, from the store's FTS index.
    pub messages: Vec<GlobalHit>,
    pub messages_loading: bool,
    /// Dropped by a newer query; the older answer must not replace it.
    _message_task: Option<Task<()>>,
}

impl Palette {
    pub(crate) fn close(&mut self, cx: &mut Context<WhatsAppApp>) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        self.query.clear();
        self.rows.clear();
        self.messages.clear();
        self.messages_loading = false;
        self.selected = 0;
        self._message_task = None;
        cx.notify();
        true
    }
}

impl WhatsAppApp {
    /// Open the command bar over whatever is on screen.
    pub(crate) fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A palette over Settings or a picture would be a second surface
        // competing for the same keyboard. Both are one Escape away, and the
        // palette is a way to leave them, not a thing to open on top of them.
        if self.showing_settings(cx) {
            self.close_settings(cx);
        }
        self.close_media_viewer(cx);
        if self.palette.open {
            self.palette.close(cx);
            return;
        }

        if self.palette.input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search chats, messages and actions…")
            });
            cx.subscribe_in(
                &input,
                window,
                |app, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let query = input.read(cx).value().to_string();
                        app.set_palette_query(query, cx);
                    }
                    InputEvent::PressEnter { .. } => app.palette_accept(window, cx),
                    _ => {}
                },
            )
            .detach();
            self.palette.input = Some(input);
        }

        self.palette.open = true;
        self.palette.selected = 0;
        // The box outlives the surface. Resetting only the query left the
        // old text on screen the next time Ctrl-K opened it, and the rows
        // that came back were for an empty query while the field showed the
        // previous search.
        self.set_palette_query(String::new(), cx);
        if let Some(input) = self.palette.input.as_ref() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn close_command_palette(&mut self, cx: &mut Context<Self>) -> bool {
        self.palette.close(cx)
    }

    pub(crate) fn set_palette_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.palette.query = query;
        self.palette.selected = 0;
        self.rebuild_palette_rows();

        let query = self.palette.query.trim().to_string();
        self.palette._message_task = None;
        if query.chars().count() < 2 {
            self.palette.messages.clear();
            self.palette.messages_loading = false;
            self.rebuild_palette_rows();
            cx.notify();
            return;
        }
        let Some(answer) = self
            .client
            .as_ref()
            .map(|client| client.search_everywhere(query.clone(), 20))
        else {
            self.palette.messages.clear();
            self.palette.messages_loading = false;
            self.rebuild_palette_rows();
            cx.notify();
            return;
        };
        self.palette.messages_loading = true;
        self.palette._message_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = answer.await;
            let _ = this.update(cx, |app, cx| {
                // The query may have moved on while the store was answering.
                if app.palette.query.trim() != query {
                    return;
                }
                app.palette.messages_loading = false;
                app.palette.messages = match result {
                    Ok(Ok(hits)) => hits
                        .into_iter()
                        .map(|(jid, message)| app.global_hit(jid, message))
                        .collect(),
                    _ => Vec::new(),
                };
                app.rebuild_palette_rows();
                cx.notify();
            });
        }));
        self.rebuild_palette_rows();
        cx.notify();
    }

    /// The chats and actions a query matches, then the message hits.
    ///
    /// Rebuilt on every keystroke. The chat half is a walk over the loaded
    /// list, which is the same list the sidebar already holds; the message
    /// half arrives later and replaces its section rather than the whole
    /// surface.
    fn rebuild_palette_rows(&mut self) {
        let query = self.palette.query.trim().to_lowercase();
        let mut rows = Vec::new();

        let mut chats: Vec<(u32, usize, PaletteRow)> = self
            .chats
            .iter()
            .enumerate()
            .filter(|(_, chat)| !chat.is_status)
            .filter_map(|(index, chat)| {
                let score = fuzzy_score(&chat.name, &query)?;
                Some((
                    score,
                    index,
                    PaletteRow {
                        kind: PaletteKind::Chat,
                        title: chat.name.clone(),
                        subtitle: chat
                            .last_message
                            .clone()
                            .unwrap_or_else(|| chat.jid.clone()),
                        jid: Some(chat.jid.clone()),
                        message_id: None,
                        action: None,
                        avatar_key: chat.avatar_cache_key.clone(),
                        unread: chat.unread_count,
                    },
                ))
            })
            .collect();
        chats.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        rows.extend(chats.into_iter().take(50).map(|(_, _, row)| row));

        for hit in &self.palette.messages {
            rows.push(PaletteRow {
                kind: PaletteKind::Message,
                title: hit.chat_name.clone(),
                subtitle: match &hit.sender {
                    Some(sender) => format!("{sender}: {}", hit.snippet),
                    None => hit.snippet.clone(),
                },
                jid: Some(hit.chat_jid.clone()),
                message_id: Some(hit.message_id.clone()),
                action: None,
                avatar_key: self
                    .find_chat(&hit.chat_jid)
                    .and_then(|chat| chat.avatar_cache_key.clone()),
                unread: 0,
            });
        }

        let mut actions = vec![
            ("New chat", "Start a conversation", PaletteAction::NewChat),
            (
                "Settings",
                "Appearance, devices and diagnostics",
                PaletteAction::Settings,
            ),
            ("Calls", "Call history", PaletteAction::Calls),
            (
                "Files & downloads",
                "Everything shared in your chats",
                PaletteAction::Files,
            ),
        ];
        if self.selected_chat.is_some() {
            actions.push((
                "Search in this chat",
                "Find a message in the open conversation",
                PaletteAction::ConversationSearch,
            ));
        }
        for (title, subtitle, action) in actions {
            if fuzzy_score(title, &query).is_some() {
                rows.push(PaletteRow {
                    kind: PaletteKind::Action,
                    title: title.to_string(),
                    subtitle: subtitle.to_string(),
                    jid: None,
                    message_id: None,
                    action: Some(action),
                    avatar_key: None,
                    unread: 0,
                });
            }
        }

        self.palette.selected = self.palette.selected.min(rows.len().saturating_sub(1));
        self.palette.rows = rows;
    }

    pub(crate) fn step_command_palette(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.palette.rows.is_empty() {
            return;
        }
        let len = self.palette.rows.len();
        self.palette.selected = if forward {
            (self.palette.selected + 1) % len
        } else {
            (self.palette.selected + len - 1) % len
        };
        cx.notify();
    }

    pub(crate) fn palette_accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.palette.rows.get(self.palette.selected).cloned() else {
            self.palette.close(cx);
            return;
        };
        self.palette.close(cx);
        match row.kind {
            PaletteKind::Chat => {
                if let Some(jid) = row.jid {
                    self.select_chat(jid, ChatOpen::ToCompose, window, cx);
                }
            }
            PaletteKind::Message => {
                if let (Some(jid), Some(message_id)) = (row.jid, row.message_id) {
                    self.open_search_hit(jid, message_id, window, cx);
                }
            }
            PaletteKind::Action => match row.action {
                Some(PaletteAction::NewChat) => self.open_new_chat(window, cx),
                Some(PaletteAction::Settings) => self.open_settings(window, cx),
                Some(PaletteAction::Calls) => self.open_call_history(cx),
                Some(PaletteAction::Files) => {
                    self.calls_open = false;
                    self.files_open = true;
                    if let Some(view) = &self.files_view {
                        view.update(cx, |view, cx| view.refresh(cx));
                    }
                    cx.notify();
                }
                Some(PaletteAction::ConversationSearch) => {
                    self.toggle_conversation_search(window, cx)
                }
                None => {}
            },
        }
        cx.notify();
    }
}

/// A cheap subsequence score. Substring matches beat scattered ones, and an
/// earlier match beats a later one; the exact number does not matter beyond
/// that ordering.
fn fuzzy_score(haystack: &str, needle: &str) -> Option<u32> {
    if needle.is_empty() {
        return Some(0);
    }
    let haystack = haystack.to_lowercase();
    let needle = needle.to_lowercase();
    if let Some(at) = haystack.find(&needle) {
        return Some(2_000_u32.saturating_sub(at as u32));
    }
    let mut score: u32 = 1_000;
    let mut from = 0;
    for ch in needle.chars() {
        let offset = haystack[from..].find(ch)?;
        score = score.saturating_sub(offset as u32);
        from += offset + ch.len_utf8();
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn a_prefix_beats_a_scattered_match() {
        let prefix = fuzzy_score("Alice", "ali").unwrap();
        let scattered = fuzzy_score("A little chat", "ali").unwrap();
        assert!(prefix > scattered);
        assert!(fuzzy_score("Alice", "zzz").is_none());
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(fuzzy_score("anything", ""), Some(0));
    }
}
