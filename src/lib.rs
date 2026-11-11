pub mod audio;
pub mod settings;
pub mod api;
pub mod input;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DictationMode {
    Transcription,
    Formal,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Phase {
    Idle,
    Recording {
        address: String,
        mode: DictationMode,
    },
    Processing {
        request_id: u64,
        mode: DictationMode,
    },
    Error,
}

impl Phase {
    pub fn start(&mut self, address: &str, mode: DictationMode) -> bool {
        if !matches!(self, Self::Idle | Self::Error) || !valid_address(address) {
            return false;
        }
        *self = Self::Recording {
            address: address.into(),
            mode,
        };
        true
    }
    pub fn stop(&mut self, request_id: u64) -> Option<(String, DictationMode)> {
        if let Self::Recording { address, mode } = self {
            let address = address.clone();
            let mode = *mode;
            *self = Self::Processing { request_id, mode };
            Some((address, mode))
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

fn strip_inline_markdown(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let is_img = chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[';
        if chars[i] == '[' || is_img {
            let bracket_start = if is_img { i + 1 } else { i };
            if let Some(close_bracket) = chars[bracket_start + 1..]
                .iter()
                .position(|&c| c == ']')
                .map(|p| bracket_start + 1 + p)
                && close_bracket + 1 < chars.len()
                && chars[close_bracket + 1] == '('
                && let Some(close_paren) = chars[close_bracket + 2..]
                    .iter()
                    .position(|&c| c == ')')
                    .map(|p| close_bracket + 2 + p)
            {
                let label: String = chars[bracket_start + 1..close_bracket].iter().collect();
                if !label.is_empty() {
                    s.push_str(&label);
                } else {
                    let url: String = chars[close_bracket + 2..close_paren].iter().collect();
                    s.push_str(&url);
                }
                i = close_paren + 1;
                continue;
            }
        }
        s.push(chars[i]);
        i += 1;
    }

    let s = s.replace('`', "");
    let s = s.replace("***", "").replace("**", "").replace("__", "");

    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut idx = 0;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '*' || ch == '_' {
            let prev_ok = idx == 0 || !chars[idx - 1].is_alphanumeric();
            if prev_ok
                && idx + 1 < chars.len()
                && !chars[idx + 1].is_whitespace()
                && let Some(rel_end) = chars[idx + 1..].iter().enumerate().find_map(|(off, &c)| {
                    if c == ch {
                        let abs_end = idx + 1 + off;
                        let inner_end_ok = abs_end > idx + 1 && !chars[abs_end - 1].is_whitespace();
                        let after_ok =
                            abs_end + 1 == chars.len() || !chars[abs_end + 1].is_alphanumeric();
                        if inner_end_ok && after_ok {
                            Some(abs_end)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
            {
                for &c in &chars[idx + 1..rel_end] {
                    out.push(c);
                }
                idx = rel_end + 1;
                continue;
            }
        }
        out.push(ch);
        idx += 1;
    }

    out
}

pub fn strip_markdown(text: &str) -> String {
    let mut out_lines: Vec<String> = Vec::new();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");

    for raw_line in normalized.split('\n') {
        let trimmed_start = raw_line.trim_start();

        if trimmed_start.starts_with("```") {
            continue;
        }

        let trimmed = raw_line.trim();
        if trimmed.len() >= 3
            && ((trimmed.chars().all(|c| c == '-' || c == ' ')
                && trimmed.chars().filter(|&c| c == '-').count() >= 3)
                || (trimmed.chars().all(|c| c == '*' || c == ' ')
                    && trimmed.chars().filter(|&c| c == '*').count() >= 3)
                || (trimmed.chars().all(|c| c == '_' || c == ' ')
                    && trimmed.chars().filter(|&c| c == '_').count() >= 3))
        {
            continue;
        }

        let mut line = trimmed_start;

        while let Some(rest) = line.strip_prefix('>') {
            line = rest.trim_start();
        }

        let hash_count = line.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hash_count) {
            let rest = &line[hash_count..];
            if rest.starts_with(' ') || rest.starts_with('\t') || rest.is_empty() {
                line = rest.trim_start();
            }
        }

        for prefix in ["* ", "+ ", "- "] {
            if let Some(rest) = line.strip_prefix(prefix) {
                line = rest.trim_start();
                break;
            }
        }

        out_lines.push(strip_inline_markdown(line));
    }

    out_lines.join("\n")
}

pub fn sanitize_text(text: &str, preserve_newlines: bool) -> String {
    let plain = strip_markdown(text);
    let mut out = String::with_capacity(plain.len());
    let mut prev_space = false;
    let mut consecutive_newlines = 0;

    for ch in plain.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            if preserve_newlines {
                if consecutive_newlines < 2 {
                    out.push('\n');
                    consecutive_newlines += 1;
                    prev_space = false;
                }
            } else if !prev_space && !out.is_empty() {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        consecutive_newlines = 0;

        let mapped = if ch == '\t' {
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
    out.trim().to_owned()
}

pub fn sanitize_plain_text(text: &str) -> String {
    sanitize_text(text, false)
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
    fn formal_text_sanitization_preserves_paragraphs() {
        assert_eq!(
            sanitize_text("Estimado Juan:\n\nEspero te encuentres bien.\r\nSaludos.", true),
            "Estimado Juan:\n\nEspero te encuentres bien.\nSaludos."
        );
    }

    #[test]
    fn markdown_is_stripped_cleanly_without_breaking_normal_text() {
        let input = "```markdown\n### Resumen del día\n\n**Asunto:** Revisión de *ventas*\n- Primer punto\n* Segundo punto\n> Nota sobre `archivo.rs` y [la web](https://example.com)\n--- \n```";
        let expected = "Resumen del día\n\nAsunto: Revisión de ventas\nPrimer punto\nSegundo punto\nNota sobre archivo.rs y la web";
        assert_eq!(sanitize_text(input, true), expected);

        // Preserve legitimate math, hashtags without space, and snake_case
        let safe = "El resultado de 2 * 3 es 6, mi_variable es #1 y el rango 10-15.";
        assert_eq!(sanitize_text(safe, false), safe);
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
