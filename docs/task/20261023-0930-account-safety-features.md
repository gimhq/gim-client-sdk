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
