//! A subscribed channel is a destination before it has any message.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

const CHANNEL: &str = "120363000000000002@newsletter";

#[tokio::test]
async fn ensure_channels_creates_the_row_and_a_name_lands_on_it() {
    let (_db, store) = test_store().await;
    let channel = jid(CHANNEL);
    assert!(store.chat(&channel).await.unwrap().is_none());

    store.ensure_channels(vec![channel.clone()]).unwrap();
    store.flush().await.unwrap();
    let row = store
        .chat(&channel)
        .await
        .unwrap()
        .expect("channel row exists before its first message");
    assert_eq!(row.name, None);

    store
        .apply_chat_names(vec![velo_chat_store::ChatNameWrite::checked(
            channel.clone(),
            None,
            "Announcements".into(),
        )])
        .unwrap();
    store.flush().await.unwrap();
    assert_eq!(
        store.chat(&channel).await.unwrap().unwrap().name.as_deref(),
        Some("Announcements")
    );

    // A second pass is idempotent: the row is still one, not a duplicate.
    store.ensure_channels(vec![channel.clone()]).unwrap();
    store.flush().await.unwrap();
    assert!(store.chat(&channel).await.unwrap().is_some());
}

#[tokio::test]
async fn an_empty_channel_survives_cleanup() {
    let (_db, store) = test_store().await;
    let channel = jid(CHANNEL);
    store.ensure_channels(vec![channel.clone()]).unwrap();
    store.flush().await.unwrap();

    let removed = store.cleanup_empty_chats().await.unwrap();

    assert_eq!(
        removed, 0,
        "a channel with no messages is still a destination"
    );
    assert!(store.chat(&channel).await.unwrap().is_some());
}
