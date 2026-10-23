//! The meta server: the privacy settings backup, reports and account
//! deletion, against one fake hub and a fake meta server.

use std::path::Path;
use std::sync::Arc;

use gim_base::model::Membership;
use gim_base::{MemoryStore, StateReader};
use gim_common::{GroupId, UserId};
use gim_identity::recovery::RecoveryCode;
use gim_identity::storage::StorageKey;
use gim_identity::RootKey;
use gim_proto::envelope::Content;
use gim_sdk::testing::{FakeMetaServer, FakeTransport, MemorySecretStorage};
use gim_sdk::{
    Client, ClientBuilder, ClientError, MetaServer, ReportReason, SecretStorage, TransportError,
    PRIVACY_SLOT, ROOT_KEY_SECRET_KEY,
};
use tempfile::TempDir;

struct Device {
    client: Client<MemoryStore>,
    store: Arc<MemoryStore>,
    secrets: Arc<MemorySecretStorage>,
    _data_dir: TempDir,
}

impl Device {
    fn new(transport: &FakeTransport, app: Option<&FakeMetaServer>) -> Self {
        let store = Arc::new(MemoryStore::new());
        let secrets = Arc::new(MemorySecretStorage::new());
        let data_dir = tempfile::tempdir().unwrap();
        let client = build(transport, app, &store, &secrets, data_dir.path());
        Self {
            client,
            store,
            secrets,
            _data_dir: data_dir,
        }
    }

    async fn register(
        transport: &FakeTransport,
        app: Option<&FakeMetaServer>,
        handle: &str,
    ) -> (Self, RecoveryCode) {
        let device = Self::new(transport, app);
        let code = device
            .client
            .register(handle, &handle.to_uppercase())
            .await
            .unwrap();
        (device, code)
    }

    fn user_id(&self) -> UserId {
        self.client.session().unwrap().device.user_id
    }

    fn root_key(&self) -> RootKey {
        let seed = self.secrets.get(ROOT_KEY_SECRET_KEY).unwrap().unwrap();
        RootKey::from_seed(&<[u8; 32]>::try_from(seed.as_slice()).unwrap())
    }

    async fn sync(&self) {
        self.client.sync_once().await.unwrap();
    }

    async fn send_text(&self, group_id: &GroupId, body: &str) {
        let content = Content::Text {
            body: body.to_string(),
        };
        self.client.send(group_id, content).await.unwrap();
    }
}

fn build(
    transport: &FakeTransport,
    app: Option<&FakeMetaServer>,
    store: &Arc<MemoryStore>,
    secrets: &Arc<MemorySecretStorage>,
    data_dir: &Path,
) -> Client<MemoryStore> {
    let builder = ClientBuilder::new(
        store.clone(),
        Arc::new(transport.clone()),
        secrets.clone() as Arc<dyn SecretStorage>,
    )
    .data_dir(data_dir);
    match app {
        Some(app) => builder.meta_server(Arc::new(app.clone())),
        None => builder,
    }
    .build()
    .unwrap()
}

#[tokio::test]
async fn blocks_and_privacy_settings_survive_the_loss_of_every_device() {
    let transport = FakeTransport::new();
    let app = FakeMetaServer::new();
    let (alice, code) = Device::register(&transport, Some(&app), "alice").await;
    let (bob, _) = Device::register(&transport, Some(&app), "bob").await;

    alice.client.block_user(&bob.user_id()).await.unwrap();
    alice.client.set_read_receipts(false).await.unwrap();
    // Sealed: the meta server sees neither the setting nor bob.
    let slot = app.slot_of(&alice.user_id(), PRIVACY_SLOT).unwrap();
    assert_eq!(slot.version, 2);
    assert!(!String::from_utf8_lossy(&slot.data).contains(bob.user_id().as_str()));
    let opened = StorageKey::derive(&alice.root_key())
        .open(PRIVACY_SLOT, &slot.data)
        .unwrap();
    assert!(String::from_utf8_lossy(&opened).contains(bob.user_id().as_str()));

    let restored = Device::new(&transport, Some(&app));
    restored
        .client
        .restore_from_recovery(&alice.user_id(), &code, true)
        .await
        .unwrap();
    let privacy = restored.client.privacy().await.unwrap();
    assert!(privacy.blocked.contains(&bob.user_id()));
    assert!(!privacy.read_receipts);
    assert!(privacy.typing);

    // A later change overwrites the backup.
    restored.client.unblock_user(&bob.user_id()).await.unwrap();
    assert_eq!(
        app.slot_of(&alice.user_id(), PRIVACY_SLOT).unwrap().version,
        3
    );
}

#[tokio::test]
async fn an_unreachable_meta_server_does_not_fail_a_setting() {
    let transport = FakeTransport::new();
    let app = FakeMetaServer::new();
    let (alice, _) = Device::register(&transport, Some(&app), "alice").await;
    app.set_unreachable(true);
    alice.client.set_typing_indicators(false).await.unwrap();
    assert!(!alice.client.privacy().await.unwrap().typing);
    app.set_unreachable(false);
    assert_eq!(app.slot_of(&alice.user_id(), PRIVACY_SLOT), None);
    // The next change writes the whole settings.
    alice.client.set_read_receipts(false).await.unwrap();
    assert!(app.slot_of(&alice.user_id(), PRIVACY_SLOT).is_some());
}

#[tokio::test]
async fn an_older_backup_is_not_written_over_a_newer_one() {
    let transport = FakeTransport::new();
    let app = FakeMetaServer::new();
    let (alice, _) = Device::register(&transport, Some(&app), "alice").await;
    let root = alice.root_key();
    let key = StorageKey::derive(&root);
    let newer = br#"{"read_receipts":true,"typing":false,"blocked":[],"updated_at_ms":18446744073709551000}"#;
    app.put_slot(
        &root,
        PRIVACY_SLOT,
        0,
        &key.seal(PRIVACY_SLOT, newer).unwrap(),
    )
    .await
    .unwrap();
    alice.client.set_read_receipts(false).await.unwrap();
    let slot = app.slot_of(&alice.user_id(), PRIVACY_SLOT).unwrap();
    assert_eq!(slot.version, 1);
}

#[tokio::test]
async fn a_report_carries_the_chosen_messages() {
    let transport = FakeTransport::new();
    let app = FakeMetaServer::new();
    let (alice, _) = Device::register(&transport, Some(&app), "alice").await;
    let (bob, _) = Device::register(&transport, None, "bob").await;
    let g = bob
        .client
        .create_direct_chat(&alice.user_id())
        .await
        .unwrap();
    alice.sync().await;
    bob.send_text(&g, "hello").await;
    bob.send_text(&g, "buy coins").await;
    alice.sync().await;
    let messages = alice.store.messages(&g, None, usize::MAX).unwrap();
    let spam = messages.last().unwrap();

    let id = alice
        .client
        .report_user(
            &bob.user_id(),
            ReportReason::Spam,
            Some("keeps sending this".into()),
            vec![spam.message_id.clone()],
        )
        .await
        .unwrap();
    assert!(!id.is_empty());
    let reports = app.reports();
    assert_eq!(reports.len(), 1);
    let (reporter, report) = &reports[0];
    assert_eq!(*reporter, alice.user_id());
    assert_eq!(report.reported_user_id, bob.user_id().as_str());
    assert_eq!(report.reason, ReportReason::Spam);
    assert_eq!(report.note.as_deref(), Some("keeps sending this"));
    assert_eq!(report.messages.len(), 1);
    assert_eq!(report.messages[0].text, "buy coins");
    assert_eq!(report.messages[0].sender_user_id, bob.user_id().as_str());

    // Not oneself; not without a meta server.
    assert!(matches!(
        alice
            .client
            .report_user(&alice.user_id(), ReportReason::Other, None, vec![])
            .await,
        Err(ClientError::InvalidInput(_))
    ));
    assert!(matches!(
        bob.client
            .report_user(&alice.user_id(), ReportReason::Other, None, vec![])
            .await,
        Err(ClientError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn a_deleted_account_is_gone_from_the_hub_and_the_meta_server() {
    let transport = FakeTransport::new();
    let app = FakeMetaServer::new();
    let (alice, code) = Device::register(&transport, Some(&app), "alice").await;
    let (bob, _) = Device::register(&transport, None, "bob").await;
    let g = bob
        .client
        .create_direct_chat(&alice.user_id())
        .await
        .unwrap();
    alice.sync().await;
    alice.client.block_user(&bob.user_id()).await.unwrap();
    assert!(app.slot_of(&alice.user_id(), PRIVACY_SLOT).is_some());

    // The meta server must be reached first: nothing is deleted without it.
    app.set_unreachable(true);
    assert!(matches!(
        alice.client.delete_account().await,
        Err(ClientError::Transport(TransportError::Network(_)))
    ));
    assert!(!transport.is_deleted(&alice.user_id()));
    app.set_unreachable(false);

    let unreached = alice.client.delete_account().await.unwrap();
    assert!(unreached.is_empty());
    assert!(transport.is_deleted(&alice.user_id()));
    assert_eq!(app.slot_of(&alice.user_id(), PRIVACY_SLOT), None);
    assert_ne!(
        alice.store.chat(&g).unwrap().unwrap().membership,
        Membership::Joined
    );
    // The recovery backup went with the account.
    let other = Device::new(&transport, Some(&app));
    assert!(matches!(
        other
            .client
            .restore_from_recovery(&alice.user_id(), &code, true)
            .await,
        Err(ClientError::InvalidInput(_))
    ));
    // Again: the hub no longer knows the user, which is not an error.
    assert!(alice.client.delete_account().await.unwrap().is_empty());
}
