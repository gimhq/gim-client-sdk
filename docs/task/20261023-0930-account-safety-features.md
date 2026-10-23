# 20261023-0930-account-safety-features Profile photo, chat settings, privacy, block/report, recovery key, account deletion

- **status**: in_progress
- **priority**: P1
- **owner**: claude/gim-sdk-safety
- **createdAt**: 2026-10-23 09:30

## Description

SDK (with gim-core and gim-hub where needed) support for the features the
Android product UI needs. Plan: `docs/plan/20261023-0930-account-safety-features.md`.

## ActiveForm

Adding account and safety features to the SDK

## Dependencies

- **blocked by**: (none)
- **blocks**: gim-android 20261023-0755-product-ui-i18n-settings (UI for these features)

## Notes

(none)
- 2026-10-23: step 1 done locally: `Client::rotate_recovery_code` and FFI
  `rotateRecoveryCode()` with 6 new tests; fmt, clippy, 392 tests and
  bindings regeneration pass. Plan revised: client-side blocking, no hub
  state except account deletion, a separate encrypted app server.
- 2026-10-23: Phase A done: privacy switches, blocking, mute/pin/archive
  and profile photos (core `dd9ac44`, SQLite migration 0008, gim-ui chat
  list fields, FFI, `docs/API.md`). Phase B (app server, reports, account
  deletion) next.
- 2026-10-23: Phase B code done (core `259c8cf`, hub `fa000fb`, new
  gimhq/gim-app, SDK app-server client, privacy backup, `report_user`,
  `delete_account`). Open: deploying gim-app to Cloudflare Workers, which
  needs credentials.
