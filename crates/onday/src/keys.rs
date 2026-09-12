//! Key names (`Enter`, `Control+A`) mapped to WebDriver key values.

use anyhow::{Result, bail};

/// A key combination: modifiers held while `key` is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub modifiers: Vec<String>,
    pub key: String,
}

/// The WebDriver value for a key name, or the character itself.
pub fn key_value(name: &str) -> Result<String> {
    let code = match name {
        "Backspace" => '\u{E003}',
        "Tab" => '\u{E004}',
        "Clear" => '\u{E005}',
        "Enter" | "Return" => '\u{E007}',
        "Shift" => '\u{E008}',
        "Control" | "Ctrl" => '\u{E009}',
        "Alt" | "Option" => '\u{E00A}',
        "Pause" => '\u{E00B}',
        "Escape" | "Esc" => '\u{E00C}',
        "Space" => ' ',
        "PageUp" => '\u{E00E}',
        "PageDown" => '\u{E00F}',
        "End" => '\u{E010}',
        "Home" => '\u{E011}',
        "ArrowLeft" | "Left" => '\u{E012}',
        "ArrowUp" | "Up" => '\u{E013}',
        "ArrowRight" | "Right" => '\u{E014}',
        "ArrowDown" | "Down" => '\u{E015}',
        "Insert" => '\u{E016}',
        "Delete" => '\u{E017}',
        "Meta" | "Command" | "Cmd" => '\u{E03D}',
        "ControlOrMeta" => {
            if cfg!(target_os = "macos") {
                '\u{E03D}'
            } else {
                '\u{E009}'
            }
        }
        other => {
            if let Some(number) = other.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
                && (1..=12).contains(&number)
            {
                return Ok(char::from_u32(0xE031 + number - 1)
                    .map(String::from)
                    .unwrap_or_default());
            }
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => c,
                _ => bail!("unknown key {other:?}"),
            }
        }
    };
    Ok(code.to_string())
}

/// Parse `Control+Shift+A`, `Enter` or `+`.
pub fn parse_chord(combo: &str) -> Result<Chord> {
    if combo.is_empty() {
        bail!("empty key combination");
    }
    let (head, key) = if combo == "+" {
        ("", "+")
    } else if let Some(head) = combo.strip_suffix("++") {
        (head, "+")
    } else {
        match combo.rsplit_once('+') {
            Some((head, key)) => (head, key),
            None => ("", combo),
        }
    };
    let modifiers = head
        .split('+')
        .filter(|part| !part.is_empty())
        .map(key_value)
        .collect::<Result<Vec<_>>>()?;
    Ok(Chord {
        modifiers,
        key: key_value(key)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_split_on_plus() {
        let chord = parse_chord("Control+Shift+A").expect("chord");
        assert_eq!(
            chord.modifiers,
            vec!["\u{E009}".to_string(), "\u{E008}".to_string()]
        );
        assert_eq!(chord.key, "A");
    }

    #[test]
    fn a_trailing_plus_is_the_plus_key() {
        let chord = parse_chord("Control++").expect("chord");
        assert_eq!(chord.key, "+");
        assert_eq!(parse_chord("+").expect("chord").key, "+");
    }

    #[test]
    fn named_keys_map_to_webdriver_values() {
        assert_eq!(key_value("Enter").expect("enter"), "\u{E007}");
        assert_eq!(key_value("F5").expect("f5"), "\u{E035}");
        assert!(key_value("NotAKey").is_err());
    }
}
