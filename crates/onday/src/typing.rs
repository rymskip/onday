//! What a field holds and whether keys can fill it, as the raw-driver helpers
//! and the locators both read it from the page runtime's `typingState`.

use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TypingKind {
    Input,
    Textarea,
    Editable,
    /// An input with a native control, such as a date or color picker.
    Native,
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TypingState {
    pub(crate) kind: TypingKind,
    #[serde(rename = "type")]
    pub(crate) input_type: Option<String>,
    pub(crate) text: String,
    pub(crate) focused: bool,
    /// The element a pointer at the field's centre lands on.
    pub(crate) on_top: String,
    /// The element holding focus.
    pub(crate) active: String,
}

impl TypingState {
    /// Refuse a `field` that typed text cannot fill.
    pub(crate) fn ensure_typable(&self, field: &str) -> Result<()> {
        match self.kind {
            TypingKind::Native => bail!(
                "{field} is an <input type={}>: text typing does not apply to that input type",
                self.input_type.as_deref().unwrap_or_default()
            ),
            TypingKind::Other => {
                bail!("{field} is not an input, textarea or contenteditable element")
            }
            TypingKind::Input | TypingKind::Textarea | TypingKind::Editable => Ok(()),
        }
    }
}

/// Whether every typed character appears in `landed`, in order. A mask's own
/// separators may sit between them; WebDriver key codepoints are keys, not text.
pub(crate) fn typed_in_order(typed: &str, landed: &str) -> bool {
    let mut remaining = landed.chars();
    typed
        .chars()
        .filter(|character| !('\u{e000}'..='\u{f8ff}').contains(character))
        .all(|character| remaining.any(|held| held == character))
}

#[cfg(test)]
mod tests {
    use super::typed_in_order;
    use thirtyfour::prelude::Key;

    #[test]
    fn exact_text_is_in_order() {
        assert!(typed_in_order("hello", "hello"));
        assert!(typed_in_order("", ""));
    }

    #[test]
    fn mask_separators_are_tolerated() {
        assert!(typed_in_order("5551234567", "(555) 123-4567"));
        assert!(typed_in_order("1234.5", "1,234.50"));
    }

    #[test]
    fn a_dropped_character_fails() {
        assert!(!typed_in_order("5551234567", "(555) 123-456"));
    }

    #[test]
    fn a_reordered_value_fails() {
        assert!(!typed_in_order("abc", "acb"));
    }

    #[test]
    fn key_codepoints_are_skipped() {
        let typed = format!("ab{}", char::from(Key::Tab));
        assert!(typed_in_order(&typed, "ab"));
    }
}
