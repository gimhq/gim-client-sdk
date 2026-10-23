//! The gim app server (gim-app): per-user storage slots sealed with a key
//! derived from the root key, and abuse reports. Every request is signed
//! with the user's root key (gim-core `docs/identity.md`, "Root-signed
//! requests"); the server keeps no account.
//!
//! The SDK keeps one slot, [`PRIVACY_SLOT`]: the privacy settings (read
//! receipts, typing indicators, blocked users), written after every change
//! and read back by [`Client::restore_from_recovery`], so blocks survive the
//! loss of every device. Per-chat settings are not backed up: a restored
//! device rejoins its chats anew.

use std::collections::BTreeSet;

use async_trait::async_trait;
use gim_base::model::{MessageContent, Privacy, StateChange};
use gim_base::{BaseError, StateReader, StateStore, StoreTxn};
use gim_common::{MessageId, UserId};
use gim_identity::storage::StorageKey;
use gim_identity::RootKey;
use serde::{Deserialize, Serialize};

use crate::transport::{TransportError, TransportResult};
use crate::{Client, ClientError, Result};

/// Slot of the privacy settings backup.
pub const PRIVACY_SLOT: &str = "privacy";
/// Most messages one report carries, as on the app server.
pub const MAX_REPORTED_MESSAGES: usize = 50;
/// Longest report note in characters, as on the app server.
pub const MAX_REPORT_NOTE: usize = 1000;
/// Longest reported message text in characters, as on the app server;
/// longer texts are cut.
const MAX_REPORTED_TEXT: usize = 4000;
/// Writes of a slot that lost against a concurrent write, before giving up.
const SLOT_WRITE_ATTEMPTS: u32 = 3;

/// A storage slot as stored: its version (from 1) and the sealed data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSlot {
    pub version: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportReason {
    Spam,
    Harassment,
    Inappropriate,
    Scam,
    Other,
}

/// A message the reporter discloses, in plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedMessage {
    pub message_id: String,
    pub sender_user_id: String,
    pub sent_at_ms: u64,
    pub text: String,
}

/// `POST /api/raw/gim/v1/reports`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbuseReport {
    pub reported_user_id: String,
    pub reason: ReportReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub messages: Vec<ReportedMessage>,
}

/// The app server, as the client sees it. Every call signs its request
/// with `root`.
#[async_trait]
pub trait AppServer: Send + Sync {
    /// The slot `name`; `None` if it was never written.
    async fn slot(&self, root: &RootKey, name: &str) -> TransportResult<Option<StoredSlot>>;

    /// Writes `data` as the version after `version` (0 creates the slot)
    /// and returns the new version; [`TransportError::Conflict`] if the
    /// slot is no longer at `version`.
    async fn put_slot(
        &self,
        root: &RootKey,
        name: &str,
        version: u64,
        data: &[u8],
    ) -> TransportResult<u64>;

    /// Files a report; returns its id.
    async fn report(&self, root: &RootKey, report: &AbuseReport) -> TransportResult<String>;

    /// Deletes every slot of the user (account deletion).
    async fn delete_user_data(&self, root: &RootKey) -> TransportResult<()>;
}

/// The privacy settings as backed up (JSON, then sealed).
#[derive(Debug, Serialize, Deserialize)]
struct PrivacyBackup {
    read_receipts: bool,
    typing: bool,
    blocked: BTreeSet<String>,
    updated_at_ms: u64,
}

impl PrivacyBackup {
    fn of(privacy: &Privacy) -> Self {
        Self {
            read_receipts: privacy.read_receipts,
            typing: privacy.typing,
            blocked: privacy.blocked.iter().map(|u| u.to_string()).collect(),
            updated_at_ms: privacy.updated_at_ms,
        }
    }

    fn privacy(self) -> Result<Privacy> {
        let blocked = self
            .blocked
            .into_iter()
            .map(UserId::try_from)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| ClientError::Identity(format!("privacy backup: {e}")))?;
        Ok(Privacy {
            read_receipts: self.read_receipts,
            typing: self.typing,
            blocked,
            updated_at_ms: self.updated_at_ms,
        })
    }
}

fn open_backup(key: &StorageKey, slot: &StoredSlot) -> Result<PrivacyBackup> {
    let plaintext = key
        .open(PRIVACY_SLOT, &slot.data)
        .map_err(ClientError::identity)?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| ClientError::Identity(format!("privacy backup: {e}")))
}

/// What a report shows of a message: its text or media caption.
fn reported_text(content: &MessageContent) -> String {
    let text = match content {
        MessageContent::Text { body } => body.as_str(),
        MessageContent::Media { caption, .. } => caption.as_deref().unwrap_or_default(),
        MessageContent::PendingMedia(_) => "",
    };
    text.chars().take(MAX_REPORTED_TEXT).collect()
}

impl<S: StateStore + 'static> Client<S> {
    fn app_server(&self) -> Result<&dyn AppServer> {
        self.inner
            .app_server
            .as_deref()
            .ok_or_else(|| ClientError::InvalidInput("no app server configured".into()))
    }

    /// Reports `user_id` to the app server's moderators, with the messages
    /// `message_ids` (from the local history) in plaintext: the server
    /// cannot read chats, so they are what the moderators see. Returns the
    /// report id. Blocking is separate ([`Client::block_user`]).
    pub async fn report_user(
        &self,
        user_id: &UserId,
        reason: ReportReason,
        note: Option<String>,
        message_ids: Vec<MessageId>,
    ) -> Result<String> {
        let session = self.require_session()?;
        if *user_id == session.device.user_id {
            return Err(ClientError::InvalidInput("cannot report yourself".into()));
        }
        if message_ids.len() > MAX_REPORTED_MESSAGES {
            return Err(ClientError::InvalidInput(format!(
                "at most {MAX_REPORTED_MESSAGES} messages per report"
            )));
        }
        if note
            .as_ref()
            .is_some_and(|n| n.chars().count() > MAX_REPORT_NOTE)
        {
            return Err(ClientError::InvalidInput(format!(
                "the note is longer than {MAX_REPORT_NOTE} characters"
            )));
        }
        let app_server = self.app_server()?;
        let messages = self
            .read(move |store, _| {
                message_ids
                    .iter()
                    .map(|id| {
                        let message = store
                            .message(id)?
                            .ok_or_else(|| BaseError::NotFound(format!("message {id}")))?;
                        Ok(ReportedMessage {
                            message_id: message.message_id.to_string(),
                            sender_user_id: message.sender.user_id.to_string(),
                            sent_at_ms: message.sent_at_ms,
                            text: reported_text(&message.content),
                        })
                    })
                    .collect::<std::result::Result<Vec<_>, BaseError>>()
            })
            .await?;
        let report = AbuseReport {
            reported_user_id: user_id.to_string(),
            reason,
            note: note.filter(|n| !n.trim().is_empty()),
            messages,
        };
        Ok(app_server.report(&self.root_key()?, &report).await?)
    }

    /// Writes the privacy settings to the app server, unless the backup
    /// there is as new. Failures are only logged: the next change writes
    /// it again.
    pub(crate) async fn back_up_privacy(&self, privacy: &Privacy) {
        let Some(app_server) = self.inner.app_server.as_deref() else {
            return;
        };
        let written = async {
            let root = self.root_key()?;
            let key = StorageKey::derive(&root);
            let plaintext = serde_json::to_vec(&PrivacyBackup::of(privacy))
                .map_err(|e| ClientError::Identity(e.to_string()))?;
            for _ in 0..SLOT_WRITE_ATTEMPTS {
                let stored = app_server.slot(&root, PRIVACY_SLOT).await?;
                let version = match &stored {
                    Some(slot) => {
                        let newer = open_backup(&key, slot)
                            .is_ok_and(|b| b.updated_at_ms >= privacy.updated_at_ms);
                        if newer {
                            return Ok(());
                        }
                        slot.version
                    }
                    None => 0,
                };
                let sealed = key
                    .seal(PRIVACY_SLOT, &plaintext)
                    .map_err(ClientError::identity)?;
                match app_server
                    .put_slot(&root, PRIVACY_SLOT, version, &sealed)
                    .await
                {
                    Err(TransportError::Conflict(_)) => continue,
                    written => return written.map(drop).map_err(ClientError::from),
                }
            }
            Err(ClientError::InvalidInput(
                "the privacy backup kept changing".into(),
            ))
        }
        .await;
        if let Err(err) = written {
            tracing::warn!(error = %err, "cannot back up the privacy settings");
        }
    }

    /// Applies the privacy settings backed up on the app server, if newer
    /// than the local ones. Failures are only logged.
    pub(crate) async fn restore_privacy(&self) {
        let Some(app_server) = self.inner.app_server.as_deref() else {
            return;
        };
        let restored = async {
            let root = self.root_key()?;
            let Some(slot) = app_server.slot(&root, PRIVACY_SLOT).await? else {
                return Ok(None);
            };
            let backup = open_backup(&StorageKey::derive(&root), &slot)?.privacy()?;
            self.run(move |store, _| {
                store.transaction(|txn| {
                    if txn.privacy()?.updated_at_ms >= backup.updated_at_ms {
                        return Ok(None);
                    }
                    txn.set_privacy(&backup)?;
                    Ok(Some(backup))
                })
            })
            .await
        }
        .await;
        match restored {
            Ok(Some(privacy)) => self.emit(&[StateChange::PrivacyChanged(privacy)]),
            Ok(None) => {}
            Err(err) => tracing::warn!(error = %err, "cannot restore the privacy settings"),
        }
    }
}
