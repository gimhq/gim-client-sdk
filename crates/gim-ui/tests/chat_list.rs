mod common;

use std::pin::pin;
use std::time::Duration;

use common::{apply, next, Device};
use gim_proto::envelope::Content;
use gim_sdk::testing::FakeTransport;
use gim_ui::{ChatKind, ChatListService, SendState, VectorDiff};

fn pause() {
    // Distinct `updated_at_ms` values.
    std::thread::sleep(Duration::from_millis(5));
}

#[tokio::test]
async fn ordered_by_update_with_minimal_diffs() {
    let transport = FakeTransport::new();
    let alice = Device::register(&transport, "alice", "Alice").await;
    let bob = Device::register(&transport, "bob", "Bob").await;
    let carol = Device::register(&transport, "carol", "Carol").await;
    bob.client.find_user("alice").await.unwrap();
    bob.client.find_user("carol").await.unwrap();

    let with_alice = alice
        .client
        .create_direct_chat(&bob.user_id())
        .await
        .unwrap();
    alice.send_text(&with_alice, "hello").await;
    bob.sync().await;
    pause();
    let trip = carol
        .client
        .create_group_chat(Some("trip".into()), &[bob.user_id()])
        .await
        .unwrap();
    carol.send_text(&trip, "hey").await;
    bob.sync().await;

    let service = ChatListService::new(&bob.client).await.unwrap();
    let (mut items, stream) = service.entries();
    let mut stream = pin!(stream);
    let ids: Vec<_> = items.iter().map(|i| i.chat_id.clone()).collect();
    assert_eq!(ids, vec![trip.clone(), with_alice.clone()]);

    let direct = &items[1];
    assert_eq!(direct.kind, ChatKind::Direct);
    assert_eq!(direct.name, "Alice");
    assert_eq!(direct.unread_count, 1);
    assert_eq!(direct.member_count, 2);
    let preview = direct.last_message.as_ref().unwrap();
    assert_eq!(
        (
            preview.sender_name.as_str(),
            preview.text.as_str(),
            preview.is_own
        ),
        ("Alice", "hello", false)
    );
    assert_eq!(preview.send_state, None);
    // The creator shares the name with the joiners.
    assert_eq!(items[0].name, "trip");
    assert_eq!(items[0].kind, ChatKind::Group);
    assert_eq!(items[0].last_message.as_ref().unwrap().text, "hey");

    // A new message moves the chat to the top: Remove + Insert.
    pause();
    let again = alice.send_text(&with_alice, "again").await;
    bob.sync().await;
    let diffs = next(&mut stream).await;
    let [VectorDiff::Remove { index: 1 }, VectorDiff::Insert { index: 0, value }] =
        diffs.as_slice()
    else {
        panic!("unexpected diffs {diffs:?}");
    };
    assert_eq!(value.chat_id, with_alice);
    assert_eq!(value.unread_count, 2);
    assert_eq!(value.last_message.as_ref().unwrap().text, "again");
    apply(&mut items, diffs);

    // Editing the last message changes the row in place.
    alice
        .send(
            &with_alice,
            Content::Edit {
                target: again,
                new_body: "again!".into(),
            },
        )
        .await;
    bob.sync().await;
    let diffs = next(&mut stream).await;
    let [VectorDiff::Set { index: 0, value }] = diffs.as_slice() else {
        panic!("unexpected diffs {diffs:?}");
    };
    assert_eq!(value.last_message.as_ref().unwrap().text, "again!");
    assert_eq!(value.unread_count, 2);
    apply(&mut items, diffs);

    // An own message in the other chat moves it back up.
    pause();
    bob.send_text(&trip, "on my way").await;
    common::wait_until(&mut stream, &mut items, |items| {
        items[0].chat_id == trip
            && items[0]
                .last_message
                .as_ref()
                .is_some_and(|m| m.is_own && m.text == "on my way")
    })
    .await;
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].chat_id, with_alice);
}

/// The preview of an own last message carries its send state, which
/// follows the receipts like the timeline ticks.
#[tokio::test]
async fn own_last_message_shows_its_send_state() {
    let transport = FakeTransport::new();
    let alice = Device::register(&transport, "alice", "Alice").await;
    let bob = Device::register(&transport, "bob", "Bob").await;
    let chat = alice
        .client
        .create_direct_chat(&bob.user_id())
        .await
        .unwrap();
    alice.send_text(&chat, "hello").await;

    let service = ChatListService::new(&alice.client).await.unwrap();
    let (mut items, stream) = service.entries();
    let mut stream = pin!(stream);
    let preview = items[0].last_message.as_ref().unwrap();
    assert!(preview.is_own);
    assert!(
        matches!(
            preview.send_state,
            Some(SendState::Pending | SendState::Sent)
        ),
        "{preview:?}"
    );

    bob.sync().await;
    bob.client.mark_read(&chat).await.unwrap();
    alice.sync().await;
    common::wait_until(&mut stream, &mut items, |items| {
        items[0]
            .last_message
            .as_ref()
            .is_some_and(|m| m.send_state == Some(SendState::Read))
    })
    .await;
}
