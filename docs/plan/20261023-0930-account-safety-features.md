# 20261023-0930-account-safety-features Profile photo, chat settings, privacy, block/report, recovery key, account deletion

- **status**: implementing
- **createdAt**: 2026-10-23 09:30
- **approvedAt**: 2026-10-23 09:30 (user: "these features are needed; start on the SDK")
- **relatedTask**: 20261023-0930-account-safety-features

## Context

The Android product-UI campaign (gim-android plan
`20261023-0755-product-ui-i18n-settings`) needs six features the SDK does
not offer yet. Where each one has to change:

| Feature | gim-core | gim-client-sdk | gim-hub |
|---|---|---|---|
| Rotate the recovery key (a lost key cannot be shown again: it is never stored) | none: `seal_root_key_backup` exists | `rotate_recovery_code`, FFI (done) | none: `PUT /v2/users/{id}/recovery-backup` already replaces |
| Own profile photo | `Profile`/`SelfProfile` envelope: optional avatar `MediaInfo` (old clients ignore the key) | upload, announce to chats and own devices, `UserProfile.avatar`, direct-chat avatar, FFI | none: blobs exist |
| Mute, pin, archive | `gim-base` `Chat` fields; `SelfChatSettings` envelope to sync own devices | store migration, chat list order and filter, push suppression in `process_push`, FFI | none |
| Read-receipt and typing switches | `SelfPreferences` envelope | preference store, stop sending (and, as in Signal, stop showing) when off, FFI | none |
| Block | `SelfPreferences` carries the block list | client-side only: refuse (leave) direct chats and invites from blocked users, drop and hide their messages, never add them, FFI | none |
| Report | none | collect the chosen messages' plaintext from the local timeline and send it to the meta server, FFI | none |
| Account deletion | `gim-identity`: root-key-signed deletion request (`gim/account-deletion/v1`) | leave every chat, delete on every hub of the hosting record, delete the meta-server data, wipe locally, FFI | `DELETE /v2/users/{id}` with the signed request: removes the account's records (device list, handle, KeyPackages, backup, hosting entry, push registration, queued events and messages) |

Design rules (user, 2026-10-23):

- gim-hub stays minimal and stateless about users' preferences: it never
  learns block lists, mute or privacy settings. Its only addition here is
  deleting a deleted account's records.
- User data that must outlive the devices (block list, chat settings,
  privacy switches, later contacts) lives on a separate **meta server** as
  ciphertext only, encrypted with a key derived from the root key; clients
  read and write it, the server cannot read it (comparable to Signal's
  storage service). Reports go to the meta server as well, not to the hub.
- Until the meta server exists, own devices stay in sync through the self
  group (MLS-encrypted), which already works without any server state.

## Proposal

Phase A (gim-core + SDK, no server change):

1. Recovery key rotation (SDK only) — done.
2. Privacy switches and block list, synced through the self group — done
   (core `e13dbf8`, `dd9ac44`: `self.privacy`; no read receipt is sent
   while they are off).
3. Mute, pin, archive, synced through the self group — done (core
   `self.chat_settings`; store migration 0008; pinned first in the chat
   list; muted chats push with `should_display = false`).
4. Own profile photo — done (`set_avatar`, `fetch_user_avatar`,
   `fetch_chat_avatar`, `ChatSummary.peer`).

Phase A findings: direct chats with a blocked user stay listed with
`ChatSummary.blocked` (as in Signal) instead of being hidden, so the user
can unblock from the chat; archive is a flag only, a new message does not
unarchive.

Phase B (servers):

5. Meta server: a new service storing per-user encrypted blobs (authenticated
   by a device-key signature or hub token, versioned writes), plus a report
   inbox for moderators. The SDK backs up and restores the Phase A state
   there, so a restore from the recovery key brings blocks and settings back.
6. Reports through the meta server.
7. Account deletion: the hub endpoint, the SDK flow, and deleting the
   meta-server data.

Phase B status: code done, deployment pending.

- Core `259c8cf`: root-signed HTTP requests (`gim/root-request/v1`,
  `RootKey::sign_request`, shared vectors) and `StorageKey` (HKDF of the
  root seed, AES-256-GCM, slot name as AAD).
- Meta server: new repository gimhq/gim-meta on zzci/bun-tpl for Cloudflare
  Workers, module `gim` (slots with versioned writes, reports, data
  deletion, moderator inbox). It authenticates by the root signature
  alone, so it holds no accounts. Not deployed yet: it needs Cloudflare
  credentials.
- Hub `fa000fb`: `DELETE /v2/users/{user_id}`, root-signed. Group rosters
  are kept, so MLS roster checks stay valid.
- SDK: `MetaServer` (`HttpMetaServer`, `FakeMetaServer`),
  `ClientBuilder::meta_server`, the privacy backup in slot `privacy`
  (written after each change, read back by `restore_from_recovery`),
  `report_user`, `delete_account`, and `Transport::delete_user`. FFI adds
  `ClientConfig.meta_server_url`, `reportUser`, `deleteAccount` and
  `ReportReason`.

Phase B findings:

- Only the privacy settings are backed up. Chat settings refer to chats
  that a fully restored device has not rejoined yet.
- Account deletion deletes the meta-server data first and the home hub
  account last. A failure in either step leaves local data intact, so the
  call can be retried.

Every core change ships as a gim-core commit that the SDK pins; every SDK
change regenerates the bindings and documents the API in `docs/API.md`.

## Risks

- Envelope additions must stay readable by current clients (unknown keys
  and content tags are ignored or rejected as specified in gim-proto).
- Client-side blocking cannot stop a blocked user from adding this user to
  a group (the hub stays unaware); the client then leaves such a direct chat
  at once and hides the blocked user's messages in groups.
- Reports carry plaintext the reporter discloses; the meta server cannot
  verify it (no message franking).
- The meta server is a new service and repository; its name, hosting and
  moderation workflow need a separate plan.
- Account deletion is irreversible: confirm in the UI, sign with the root key.

## Scope

In: the seven items above across gim-core, gim-client-sdk, gim-hub (account deletion only) and a new meta server.
Out: Android and iOS UI (their own campaigns consume the SDK release).
