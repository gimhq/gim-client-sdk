//! The recovery kit: the user id and the recovery code written together, so
//! one copy, file or printout restores the account. The labels are the
//! app's (localized); parsing ignores them, so a kit in any language, or
//! typed by hand, still parses.

use std::str::FromStr;

use gim_identity::recovery::RecoveryCode;

/// Symbols of a recovery code (24 bytes in Crockford base32).
pub const RECOVERY_CODE_SYMBOLS: usize = 39;
/// Symbols the user types back to show they saved the kit.
pub const RECOVERY_KEY_ENDING_SYMBOLS: usize = 4;
const GROUP: usize = 4;

/// The localized words of the kit text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryKitLabels {
    pub title: String,
    pub account_id: String,
    pub recovery_key: String,
    pub note: String,
}

/// What [`parse_recovery_kit`] found: the normalized, valid user id and
/// recovery code symbols (no separators), each if present.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedRecoveryKit {
    pub user_id: Option<String>,
    pub recovery_code: Option<String>,
}

/// The kit text: the title, `"<account_id>: <user id>"`,
/// `"<recovery_key>: <code grouped by four>"`, a blank line and the note,
/// each line ending in `\n`. The user id is trimmed and lowercased, the
/// code normalized as by [`normalize_recovery_code`].
pub fn format_recovery_kit(
    user_id: &str,
    recovery_code: &str,
    labels: &RecoveryKitLabels,
) -> String {
    let symbols = normalize_recovery_code(recovery_code);
    let grouped = symbols
        .chars()
        .collect::<Vec<_>>()
        .chunks(GROUP)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-");
    format!(
        "{}\n{}: {}\n{}: {}\n\n{}\n",
        labels.title,
        labels.account_id,
        user_id.trim().to_lowercase(),
        labels.recovery_key,
        grouped,
        labels.note,
    )
}

/// The code's symbols as gim-core reads them: `-` and whitespace dropped,
/// uppercased, `I`/`L` to `1` and `O` to `0`. Other characters are kept.
pub fn normalize_recovery_code(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .map(|c| match c {
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        })
        .collect()
}

/// `"u"` + 55 symbols of lowercase base32, the last one carrying 3 zero
/// bits: the shape of a user id.
fn is_user_id(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() == 56
        && bytes[0] == b'u'
        && bytes[1..55]
            .iter()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(b))
        && matches!(bytes[55], b'a' | b'i' | b'q' | b'y')
}

fn is_valid_code(symbols: &str) -> bool {
    symbols.len() == RECOVERY_CODE_SYMBOLS && RecoveryCode::from_str(symbols).is_ok()
}

/// Finds the user id and the recovery code in `text` without the labels.
/// Tokens are runs of letters and digits; the code is one token or tokens
/// joined by a single `-` or space, holding exactly 39 symbols. The user
/// id token is never part of the code. The first of each is returned.
pub fn parse_recovery_kit(text: &str) -> ParsedRecoveryKit {
    let mut tokens: Vec<(usize, usize)> = vec![];
    let mut start = None;
    for (i, c) in text.char_indices() {
        let word = c.is_alphabetic() || c.is_numeric();
        match (word, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                tokens.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        tokens.push((s, text.len()));
    }
    let token = |i: usize| &text[tokens[i].0..tokens[i].1];
    let user_ids: Vec<bool> = (0..tokens.len())
        .map(|i| is_user_id(&token(i).to_lowercase()))
        .collect();
    let user_id = user_ids
        .iter()
        .position(|u| *u)
        .map(|i| token(i).to_lowercase());

    // Chains of tokens joined by a single '-' or space, split at user ids.
    let mut chains: Vec<Vec<&str>> = vec![];
    for i in 0..tokens.len() {
        if user_ids[i] {
            chains.push(vec![]);
            continue;
        }
        let joined =
            i > 0 && !user_ids[i - 1] && matches!(&text[tokens[i - 1].1..tokens[i].0], "-" | " ");
        if chains.is_empty() || !joined {
            chains.push(vec![]);
        }
        chains.last_mut().expect("pushed above").push(token(i));
    }
    let recovery_code = chains
        .iter()
        .find_map(|chain| (0..chain.len()).find_map(|start| code_starting_at(chain, start)));
    ParsedRecoveryKit {
        user_id,
        recovery_code,
    }
}

/// The valid code made of `chain`'s tokens from `start` holding exactly
/// [`RECOVERY_CODE_SYMBOLS`] characters, if any.
fn code_starting_at(chain: &[&str], start: usize) -> Option<String> {
    let mut length = 0;
    for end in start..chain.len() {
        length += chain[end].chars().count();
        if length > RECOVERY_CODE_SYMBOLS {
            return None;
        }
        if length == RECOVERY_CODE_SYMBOLS {
            let symbols = normalize_recovery_code(&chain[start..=end].concat());
            return is_valid_code(&symbols).then_some(symbols);
        }
    }
    None
}

/// The last four symbols of the recovery code.
pub fn recovery_key_ending(recovery_code: &str) -> String {
    let symbols: Vec<char> = normalize_recovery_code(recovery_code).chars().collect();
    let from = symbols.len().saturating_sub(RECOVERY_KEY_ENDING_SYMBOLS);
    symbols[from..].iter().collect()
}

/// `typed` is the code's ending: four symbols, normalized like the code
/// (case, `-` and whitespace do not matter).
pub fn matches_recovery_key_ending(recovery_code: &str, typed: &str) -> bool {
    let typed = normalize_recovery_code(typed);
    typed.chars().count() == RECOVERY_KEY_ENDING_SYMBOLS
        && typed == recovery_key_ending(recovery_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "7BQ2-K9MX-4TVA-0HZR-PC63-W8NE-1SJD-5GFY-QK2M-V7R";

    fn user_id() -> String {
        let alphabet = "abcdefghijklmnopqrstuvwxyz234567".repeat(2);
        format!("u{}q", &alphabet[..54])
    }

    fn symbols() -> String {
        CODE.replace('-', "")
    }

    fn full() -> ParsedRecoveryKit {
        ParsedRecoveryKit {
            user_id: Some(user_id()),
            recovery_code: Some(symbols()),
        }
    }

    fn parsed(user_id: Option<String>, code: Option<String>) -> ParsedRecoveryKit {
        ParsedRecoveryKit {
            user_id,
            recovery_code: code,
        }
    }

    fn labels(title: &str, account_id: &str, recovery_key: &str, note: &str) -> RecoveryKitLabels {
        RecoveryKitLabels {
            title: title.into(),
            account_id: account_id.into(),
            recovery_key: recovery_key.into(),
            note: note.into(),
        }
    }

    fn english() -> RecoveryKitLabels {
        labels(
            "Recovery kit",
            "Account ID",
            "Recovery key",
            "Keep this safe.",
        )
    }

    #[test]
    fn format_writes_the_kit_lines() {
        let text = format_recovery_kit(
            &format!(" {} ", user_id().to_uppercase()),
            &symbols().to_lowercase(),
            &english(),
        );
        assert_eq!(
            text,
            format!(
                "Recovery kit\nAccount ID: {}\nRecovery key: {CODE}\n\nKeep this safe.\n",
                user_id()
            )
        );
    }

    #[test]
    fn format_then_parse_round_trips() {
        for code in [CODE.to_string(), symbols()] {
            assert_eq!(
                parse_recovery_kit(&format_recovery_kit(&user_id(), &code, &english())),
                full()
            );
        }
    }

    #[test]
    fn parse_ignores_the_labels() {
        let spanish = labels(
            "Kit de recuperación",
            "ID de cuenta",
            "Clave de recuperación",
            "Guárdalo.",
        );
        let chinese = labels("恢复工具包", "账户 ID", "恢复密钥", "请妥善保管。");
        for labels in [spanish, chinese] {
            assert_eq!(
                parse_recovery_kit(&format_recovery_kit(&user_id(), CODE, &labels)),
                full()
            );
        }
        let full_width = format!("账户 ID：{}\n恢复密钥：{CODE}\n", user_id());
        assert_eq!(parse_recovery_kit(&full_width), full());
    }

    #[test]
    fn parse_accepts_lowercase_and_spaced_keys() {
        let uid = user_id();
        let lower = format!("Account ID: {uid}\nRecovery key: {}\n", CODE.to_lowercase());
        assert_eq!(parse_recovery_kit(&lower), full());
        let spaced = format!(
            "Account ID: {uid}\nRecovery key: {}\n",
            CODE.replace('-', " ")
        );
        assert_eq!(parse_recovery_kit(&spaced), full());
        assert_eq!(parse_recovery_kit(&format!("{uid} {}", symbols())), full());
    }

    #[test]
    fn parse_finds_the_kit_in_surrounding_text() {
        let kit = format_recovery_kit(&user_id(), CODE, &english()).replace('\n', "\r\n");
        let text = format!(
            "Hi,\r\n\r\nhere is my kit:\r\n\r\n{kit}\r\nmy key is {} again.\r\n",
            CODE.replace('-', " ")
        );
        assert_eq!(parse_recovery_kit(&text), full());
        let upper = format!(
            "Account ID: {}\r\nRecovery key: {CODE}\r\n",
            user_id().to_uppercase()
        );
        assert_eq!(parse_recovery_kit(&upper), full());
    }

    #[test]
    fn parse_returns_whatever_part_is_there() {
        assert_eq!(
            parse_recovery_kit(&format!("my key is {CODE}")),
            parsed(None, Some(symbols()))
        );
        assert_eq!(
            parse_recovery_kit(&format!("Account ID: {}", user_id())),
            parsed(Some(user_id()), None)
        );
        for empty in ["", "  \n\t ", "hello, this is not a kit at all!"] {
            assert_eq!(parse_recovery_kit(empty), parsed(None, None));
        }
    }

    #[test]
    fn parse_rejects_bad_tokens() {
        let bad_padding = format!("{}S", &CODE[..CODE.len() - 1]);
        assert_eq!(
            parse_recovery_kit(&format!("{}\n{bad_padding}", user_id())),
            parsed(Some(user_id()), None)
        );
        // A user id glued to other letters is not a whole token.
        assert_eq!(
            parse_recovery_kit(&format!("x{}", user_id())),
            parsed(None, None)
        );
    }

    #[test]
    fn parse_never_takes_the_user_id_as_the_key() {
        assert_eq!(
            parse_recovery_kit(&format!("{}-0000", user_id())),
            parsed(Some(user_id()), None)
        );
        assert_eq!(parse_recovery_kit(&format!("{}-{CODE}", user_id())), full());
    }

    #[test]
    fn a_generated_code_round_trips() {
        let code = RecoveryCode::generate().unwrap().to_string();
        let kit = format_recovery_kit(&user_id(), &code, &english());
        assert_eq!(
            parse_recovery_kit(&kit).recovery_code,
            Some(code.replace('-', ""))
        );
    }

    #[test]
    fn key_ending_is_the_last_four_symbols() {
        assert_eq!(recovery_key_ending(CODE), "MV7R");
        assert_eq!(recovery_key_ending(&symbols().to_lowercase()), "MV7R");
    }

    #[test]
    fn key_ending_match_ignores_case_and_separators() {
        let code = format!("{}ABCD", &symbols()[..symbols().len() - 4]);
        for typed in ["ABCD", "abcd", "ab-cd", " a b c d "] {
            assert!(matches_recovery_key_ending(&code, typed), "{typed}");
        }
        for typed in ["ABCE", "BCD", "ABCDE", ""] {
            assert!(!matches_recovery_key_ending(&code, typed), "{typed}");
        }
    }
}
