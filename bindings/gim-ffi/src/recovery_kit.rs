//! The recovery kit text (see `docs/API.md`, *Recovery kit*).

use gim_sdk::recovery_kit;

/// The app's localized words of the kit text.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecoveryKitLabels {
    pub title: String,
    pub account_id: String,
    pub recovery_key: String,
    pub note: String,
}

/// What `parse_recovery_kit` found: the normalized, valid user id and
/// recovery code (39 symbols, no separators), each if present.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ParsedRecoveryKit {
    pub user_id: Option<String>,
    pub recovery_code: Option<String>,
}

/// The kit text: title, `"<accountId>: <user id>"`,
/// `"<recoveryKey>: <code grouped by four>"`, a blank line and the note.
#[uniffi::export]
pub fn format_recovery_kit(
    user_id: String,
    recovery_code: String,
    labels: RecoveryKitLabels,
) -> String {
    recovery_kit::format_recovery_kit(
        &user_id,
        &recovery_code,
        &recovery_kit::RecoveryKitLabels {
            title: labels.title,
            account_id: labels.account_id,
            recovery_key: labels.recovery_key,
            note: labels.note,
        },
    )
}

/// Finds the user id and the recovery code in pasted text, whatever the
/// labels' language.
#[uniffi::export]
pub fn parse_recovery_kit(text: String) -> ParsedRecoveryKit {
    let parsed = recovery_kit::parse_recovery_kit(&text);
    ParsedRecoveryKit {
        user_id: parsed.user_id,
        recovery_code: parsed.recovery_code,
    }
}

/// The last four symbols of the recovery code.
#[uniffi::export]
pub fn recovery_key_ending(recovery_code: String) -> String {
    recovery_kit::recovery_key_ending(&recovery_code)
}

/// `typed` is the code's last four symbols (case, `-` and spaces ignored).
#[uniffi::export]
pub fn matches_recovery_key_ending(recovery_code: String, typed: String) -> bool {
    recovery_kit::matches_recovery_key_ending(&recovery_code, &typed)
}
