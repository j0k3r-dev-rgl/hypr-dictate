#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DictationMode {
    #[default]
    Transcription,
    Formal,
}

#[derive(Debug, PartialEq)]
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

pub fn transient_wayland_error(error: &wayland_client::backend::WaylandError) -> bool {
    matches!(error, wayland_client::backend::WaylandError::Io(error) if error.kind() == std::io::ErrorKind::WouldBlock)
}

pub fn capture_valid(code: Option<i32>, signal: Option<i32>, stderr: &[u8], pcm: &[u8]) -> bool {
    pcm.len() >= 3200
        && pcm.len() <= 9_600_000
        && pcm.len().is_multiple_of(2)
        && (code == Some(0)
            || code == Some(130)
            || signal == Some(libc::SIGINT)
            || (code == Some(1) && stderr.is_empty()))
}

pub fn focus_dispatch(address: &str) -> String {
    format!("hl.dsp.focus({{window = \"address:{address}\"}})")
}

pub const SAFE_EVDEV_KEYS: [u32; 10] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

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

fn remove_inline_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let previous = out.chars().next_back();
        let mut consumed = false;
        for marker in ["**", "__", "~~", "*", "_", "`"] {
            if previous.is_some_and(char::is_alphanumeric) {
                continue;
            }
            if let Some(inner) = rest.strip_prefix(marker)
                && let Some(end) = inner.find(marker)
            {
                let content = &inner[..end];
                let after = &inner[end + marker.len()..];
                if !content.is_empty()
                    && !content.starts_with(char::is_whitespace)
                    && !content.ends_with(char::is_whitespace)
                    && !after.starts_with(char::is_alphanumeric)
                {
                    out.push_str(&remove_inline_markdown(content));
                    rest = after;
                    consumed = true;
                    break;
                }
            }
        }
        if consumed {
            continue;
        }
        if let Some(label) = rest.strip_prefix('[')
            && let Some(end) = label.find("](")
            && let Some(close) = label[end + 2..].find(')')
        {
            out.push_str(&label[..end]);
            out.push_str(" (");
            out.push_str(&label[end + 2..end + 2 + close]);
            out.push(')');
            rest = &label[end + 2 + close + 1..];
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

pub fn sanitize_redaction_text(text: &str) -> String {
    let normalized: String = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect();
    let mut lines = Vec::new();
    for line in normalized.split('\n') {
        let content = line.trim_start_matches([' ', '\t']);
        let indentation = &line[..line.len() - content.len()];
        if content.starts_with("```") || content.starts_with("~~~") {
            continue;
        }
        let mut plain = content;
        let hashes = plain.bytes().take_while(|ch| *ch == b'#').count();
        if (1..=6).contains(&hashes) && plain[hashes..].starts_with(' ') {
            plain = plain[hashes..].trim_start();
        }
        while let Some(quoted) = plain.strip_prefix("> ") {
            plain = quoted;
        }
        for bullet in ["- ", "* ", "+ "] {
            if let Some(item) = plain.strip_prefix(bullet) {
                plain = item;
                break;
            }
        }
        lines.push(format!("{indentation}{}", remove_inline_markdown(plain)));
    }
    let start = lines.iter().position(|line| !line.trim().is_empty());
    let end = lines.iter().rposition(|line| !line.trim().is_empty());
    match (start, end) {
        (Some(start), Some(end)) => lines[start..=end].join("\n"),
        _ => String::new(),
    }
}

pub fn insertion_uses_clipboard(mode: DictationMode, xwayland: bool) -> bool {
    mode == DictationMode::Formal || xwayland
}

#[cfg(test)]
mod tests {
    #[test]
    fn redaction_preserves_paragraphs_tabs_indentation_and_legitimate_content() {
        let input = "Fecha: 12/11/2026\r\n\r\n\tAsunto: Solicitud de recursos\r\n  Se solicitan 2 equipos.\n\nDice: \"servicio validado\"; 2 * 3 = 6; archivo_datos; #123.\0\u{1b}";
        let expected = "Fecha: 12/11/2026\n\n\tAsunto: Solicitud de recursos\n  Se solicitan 2 equipos.\n\nDice: \"servicio validado\"; 2 * 3 = 6; archivo_datos; #123.";
        assert_eq!(super::sanitize_redaction_text(input), expected);
    }

    #[test]
    fn redaction_removes_markdown_presentation_without_flattening_the_document() {
        let input = "```text\n# Solicitud\n\n**Asunto:** Recursos _informáticos_\n\t- Dos equipos\n> Servicio `validado`\n[Referencia](https://example.com)\n```";
        let expected = "Solicitud\n\nAsunto: Recursos informáticos\n\tDos equipos\nServicio validado\nReferencia (https://example.com)";
        assert_eq!(super::sanitize_redaction_text(input), expected);
        assert_eq!(super::sanitize_redaction_text(" \n\t\r\n"), "");
    }

    #[test]
    fn redaction_pastes_on_all_targets_while_dictation_only_pastes_on_xwayland() {
        assert!(super::insertion_uses_clipboard(
            super::DictationMode::Formal,
            false
        ));
        assert!(super::insertion_uses_clipboard(
            super::DictationMode::Formal,
            true
        ));
        assert!(super::insertion_uses_clipboard(
            super::DictationMode::Transcription,
            true
        ));
        assert!(!super::insertion_uses_clipboard(
            super::DictationMode::Transcription,
            false
        ));
    }

    #[test]
    fn redaction_preserves_numbered_sections_urls_and_quoted_content() {
        let text = "Informe\n\n1. Servicio validado\n\t2 equipos; 100 100\n\nReferencia: https://example.com\nDijo: «listo» y \"aprobado\".\nA_B, #123, 2 * 3.";
        assert_eq!(super::sanitize_redaction_text(text), text);
        assert_eq!(super::sanitize_plain_text("a\n\nb\tc"), "a b c");
    }

    #[test]
    fn plain_text_sanitization_preserves_repeated_numbers() {
        for text in [
            "El código es 5 5 1 2",
            "Teléfono: 6 6 6 1 1 2 2 3 3",
            "Importe: 100 100",
        ] {
            assert_eq!(super::sanitize_plain_text(text), text);
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
            assert_eq!(super::sanitize_plain_text(text), text);
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
            assert_eq!(super::sanitize_plain_text(text), text);
        }
    }
    #[test]
    fn plain_text_sanitization_only_normalizes_controls_and_spaces() {
        assert_eq!(
            super::sanitize_plain_text("  El código\tes 5\n5\r\n1  2...\0\u{1b}\u{7f}  "),
            "El código es 5 5 1 2..."
        );
        assert_eq!(super::sanitize_plain_text(" \n\r\t\0\u{1b}\u{7f}"), "");
        assert_eq!(super::sanitize_plain_text(""), "");
    }
    #[test]
    fn plain_text_sanitization_removes_newlines_tabs_and_dangerous_keycodes() {
        assert_eq!(
            super::sanitize_plain_text("Hola\nmundo\tbonito\r\n"),
            "Hola mundo bonito"
        );
        // Must never include ESC(1), TAB(15), Q(16), W(17), ENTER(28), F4(62)
        for forbidden in [1, 14, 15, 16, 17, 28, 62] {
            assert!(!super::SAFE_EVDEV_KEYS.contains(&forbidden));
        }
    }
    #[test]
    fn focus_uses_current_hyprland_dispatch_syntax() {
        assert_eq!(
            super::focus_dispatch("0xabc"),
            "hl.dsp.focus({window = \"address:0xabc\"})"
        );
    }
    #[test]
    fn pipewire_exit_one_requires_valid_audio_and_no_error() {
        assert!(super::capture_valid(Some(1), None, b"", &[0; 3200]));
        assert!(!super::capture_valid(
            Some(1),
            None,
            b"connection failed",
            &[0; 3200]
        ));
        assert!(!super::capture_valid(Some(1), None, b"", &[]));
        assert!(!super::capture_valid(Some(2), None, b"", &[0; 3200]));
    }
    #[test]
    fn nonblocking_wayland_read_is_not_fatal() {
        use wayland_client::backend::WaylandError;
        assert!(super::transient_wayland_error(&WaylandError::Io(
            std::io::ErrorKind::WouldBlock.into()
        )));
        assert!(!super::transient_wayland_error(&WaylandError::Io(
            std::io::ErrorKind::BrokenPipe.into()
        )));
    }
    use super::*;
    #[test]
    fn target_is_fixed_until_completion() {
        let mut phase = Phase::Idle;
        assert!(phase.start("0xabc"));
        assert!(!phase.start("0xdef"));
        assert_eq!(phase.stop(1), Some("0xabc".into()));
        assert_eq!(phase, Phase::Processing(1));
        assert_eq!(phase.stop(2), None);
    }
    #[test]
    fn invalid_target_never_starts_recording() {
        for address in ["", "0x0", "0xabc;evil", "not-an-address"] {
            let mut phase = Phase::Idle;
            assert!(!phase.start(address));
        }
    }
    #[test]
    fn wav_has_correct_lengths_and_format() {
        let audio = wav(&[0; 320]);
        assert_eq!(&audio[..4], b"RIFF");
        assert_eq!(audio.len(), 364);
        assert_eq!(u32::from_le_bytes(audio[4..8].try_into().unwrap()), 356);
        assert_eq!(u32::from_le_bytes(audio[24..28].try_into().unwrap()), 16000);
        assert_eq!(u32::from_le_bytes(audio[40..44].try_into().unwrap()), 320);
    }
}
