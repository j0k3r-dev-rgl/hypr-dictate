pub mod audio;
pub mod settings;
pub mod api;
pub mod input;

#[derive(Debug, PartialEq, Clone)]
pub enum Phase {
    Idle,
    Recording(String),
    Processing(u64),
    Error,
}

impl Phase {
    pub fn start(&mut self, address: &str) -> bool {
        if !matches!(self, Self::Idle | Self::Error) || !valid_address(address) {
            return false;
        }
        *self = Self::Recording(address.into());
        true
    }
    pub fn stop(&mut self, request_id: u64) -> Option<String> {
        if let Self::Recording(address) = self {
            let address = address.clone();
            *self = Self::Processing(request_id);
            Some(address)
        } else {
            None
        }
    }
}

pub fn valid_address(address: &str) -> bool {
    address.strip_prefix("0x").is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 16
            && s.bytes().all(|b| b.is_ascii_hexdigit())
            && s.bytes().any(|b| b != b'0')
    })
}

pub fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&32000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

pub fn capture_valid(pcm: &[u8]) -> bool {
    pcm.len() >= 3200
        && pcm.len() <= 9_600_000
        && pcm.len().is_multiple_of(2)
}

pub fn sanitize_plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        let mapped = if matches!(ch, '\n' | '\r' | '\t') {
            ' '
        } else if ch.is_control() {
            continue;
        } else {
            ch
        };
        if mapped == ' ' {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(mapped);
            prev_space = false;
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_sanitization_preserves_repeated_numbers() {
        for text in [
            "El código es 5 5 1 2",
            "Teléfono: 6 6 6 1 1 2 2 3 3",
            "Importe: 100 100",
        ] {
            assert_eq!(sanitize_plain_text(text), text);
        }
    }

    #[test]
    fn plain_text_sanitization_preserves_legitimate_repeated_words() {
        for text in [
            "Es muy muy bueno",
            "No, no quiero",
            "I had had enough",
            "Está está aquí",
            "Sí, si quieres",
            "Genial. Genial, funciona muy bien.",
        ] {
            assert_eq!(sanitize_plain_text(text), text);
        }
    }

    #[test]
    fn plain_text_sanitization_preserves_punctuation_and_unicode() {
        for text in [
            "Bueno... no sé",
            "¿En serio?? ¡Sí!!",
            "Texto ; con : signos , separados .",
            "你好，你好… 👋👋",
        ] {
            assert_eq!(sanitize_plain_text(text), text);
        }
    }

    #[test]
    fn plain_text_sanitization_only_normalizes_controls_and_spaces() {
        assert_eq!(
            sanitize_plain_text("  El código\tes 5\n5\r\n1  2...\0\u{1b}\u{7f}  "),
            "El código es 5 5 1 2..."
        );
        assert_eq!(sanitize_plain_text(" \n\r\t\0\u{1b}\u{7f}"), "");
        assert_eq!(sanitize_plain_text(""), "");
    }

    #[test]
    fn plain_text_sanitization_removes_newlines_tabs_and_dangerous_keycodes() {
        assert_eq!(
            sanitize_plain_text("Hola\nmundo\tbonito\r\n"),
            "Hola mundo bonito"
        );
    }

    #[test]
    fn wav_header_generation() {
        let pcm = vec![0u8; 3200];
        let w = wav(&pcm);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(&w[12..16], b"fmt ");
        assert_eq!(&w[36..40], b"data");
        assert_eq!(w.len(), 44 + 3200);
    }

    #[test]
    fn valid_address_checks() {
        assert!(valid_address("0x1234abcd"));
        assert!(valid_address("0x1"));
        assert!(!valid_address(""));
        assert!(!valid_address("0x0000000000000000"));
        assert!(!valid_address("1234abcd"));
    }
}
