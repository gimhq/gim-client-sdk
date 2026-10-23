//! The user's privacy settings (read receipts, typing indicators, blocked
//! users) and per-chat settings (muted, pinned, archived). Both are stored
//! locally and synced to the other own devices through the self group as
//! `self.privacy` and `self.chat_settings`; the latest change wins. Nothing
//! reaches a hub in the clear.
//!
//! Blocking is enforced on this side only: gim-base drops what a blocked
//! user sends and refuses their Welcomes, and this client creates no chat
//! with them and adds them to none.

use gim_base::model::{Chat, Privacy, StateChange};
use gim_base::{BaseError, StateReader, StateStore, StoreTxn};
use gim_common::{now_ms, GroupId, UserId};
use gim_proto::envelope::Content;

use crate::{Client, ClientError, Result};

/// `muted_until_ms` of a chat muted until it is unmuted.
pub const MUTED_FOREVER: u64 = u64::MAX;

impl<S: StateStore + 'static> Client<S> {
    /// The privacy settings; the defaults until changed.
    pub async fn privacy(&self) -> Result<Privacy> {
        self.read(|store, _| Ok(store.privacy()?)).await
    }

    /// Whether to send read receipts. Off, the others' read receipts are
    /// not shown either.
    pub async fn set_read_receipts(&self, enabled: bool) -> Result<()> {
        self.update_privacy(move |p| p.read_receipts = enabled)
            .await
    }

    /// Whether to send typing indicators. Off, the others' are not shown
    /// either.
    pub async fn set_typing_indicators(&self, enabled: bool) -> Result<()> {
        self.update_privacy(move |p| p.typing = enabled).await
    }

    /// Blocks `user_id`: their messages and invites are dropped from now
    /// on, and no chat is created with them. Existing chats stay.
    pub async fn block_user(&self, user_id: &UserId) -> Result<()> {
        let session = self.require_session()?;
        if *user_id == session.device.user_id {
            return Err(ClientError::InvalidInput("cannot block yourself".into()));
        }
        let user_id = user_id.clone();
        self.update_privacy(move |p| {
            p.blocked.insert(user_id);
        })
        .await
    }

    pub async fn unblock_user(&self, user_id: &UserId) -> Result<()> {
        let user_id = user_id.clone();
        self.update_privacy(move |p| {
            p.blocked.remove(&user_id);
        })
        .await
    }

    /// Fails with `InvalidInput` if one of `users` is blocked.
    pub(crate) async fn refuse_blocked(&self, users: &[UserId]) -> Result<()> {
        let privacy = self.privacy().await?;
        match users.iter().find(|u| privacy.blocked.contains(*u)) {
            Some(user_id) => Err(ClientError::InvalidInput(format!(
                "{user_id} is blocked; unblock them first"
            ))),
            None => Ok(()),
        }
    }

    /// Mutes the chat's notifications until `muted_until_ms` (Unix
    /// milliseconds; [`MUTED_FOREVER`] for good, 0 to unmute).
    pub async fn set_chat_muted(&self, group_id: &GroupId, muted_until_ms: u64) -> Result<()> {
        self.update_chat_settings(group_id, move |c| c.muted_until_ms = muted_until_ms)
            .await
    }

    /// Pinned chats come first in the chat list.
    pub async fn set_chat_pinned(&self, group_id: &GroupId, pinned: bool) -> Result<()> {
        self.update_chat_settings(group_id, move |c| c.pinned = pinned)
            .await
    }

    /// Archived chats are listed apart; a new message does not unarchive
    /// them.
    pub async fn set_chat_archived(&self, group_id: &GroupId, archived: bool) -> Result<()> {
        self.update_chat_settings(group_id, move |c| c.archived = archived)
            .await
    }

    async fn update_privacy(
        &self,
        change: impl FnOnce(&mut Privacy) + Send + 'static,
    ) -> Result<()> {
        self.require_session()?;
        let privacy = self
            .run(move |store, _| {
                store.transaction(|txn| {
                    let mut privacy = txn.privacy()?;
                    change(&mut privacy);
                    privacy.updated_at_ms = now_ms().max(privacy.updated_at_ms + 1);
                    txn.set_privacy(&privacy)?;
                    Ok(privacy)
                })
            })
            .await?;
        self.emit(&[StateChange::PrivacyChanged(privacy.clone())]);
        self.back_up_privacy(&privacy).await;
        self.send_to_self_group(privacy_content(&privacy)).await
    }

    async fn update_chat_settings(
        &self,
        group_id: &GroupId,
        change: impl FnOnce(&mut Chat) + Send + 'static,
    ) -> Result<()> {
        self.require_session()?;
        let gid = group_id.clone();
        let chat = self
            .run(move |store, _| {
                store.transaction(|txn| {
                    let mut chat = txn
                        .chat(&gid)?
                        .ok_or_else(|| BaseError::NotFound(format!("chat {gid}")))?;
                    change(&mut chat);
                    chat.settings_at_ms = now_ms().max(chat.settings_at_ms + 1);
                    txn.upsert_chat(&chat)?;
                    Ok(chat)
                })
            })
            .await?;
        self.emit(&[StateChange::ChatUpserted(chat.clone())]);
        self.send_to_self_group(chat_settings_content(&chat)).await
    }

    /// Shares the privacy settings and every changed chat setting with a
    /// newly linked own device.
    pub(crate) async fn share_settings(&self) -> Result<()> {
        let (privacy, chats) = self
            .read(|store, _| Ok((store.privacy()?, store.chats()?)))
            .await?;
        if privacy.updated_at_ms > 0 {
            self.send_to_self_group(privacy_content(&privacy)).await?;
        }
        for chat in chats.iter().filter(|c| c.settings_at_ms > 0) {
            self.send_to_self_group(chat_settings_content(chat)).await?;
        }
        Ok(())
    }
}

fn privacy_content(privacy: &Privacy) -> Content {
    Content::SelfPrivacy {
        read_receipts: privacy.read_receipts,
        typing: privacy.typing,
        blocked: privacy.blocked.iter().cloned().collect(),
    }
}

fn chat_settings_content(chat: &Chat) -> Content {
    Content::SelfChatSettings {
        group_id: chat.group_id.clone(),
        muted_until_ms: chat.muted_until_ms,
        pinned: chat.pinned,
        archived: chat.archived,
    }
}
