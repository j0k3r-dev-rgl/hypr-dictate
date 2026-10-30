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

fn normalize_char(c: char) -> char {
    match c {
        'á' | 'Á' => 'a',
        'é' | 'É' => 'e',
        'í' | 'Í' => 'i',
        'ó' | 'Ó' => 'o',
        'ú' | 'Ú' | 'ü' | 'Ü' => 'u',
        other => other.to_ascii_lowercase(),
    }
}

pub fn clean_repeated_words(text: &str) -> String {
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut is_word: Option<bool> = None;

    for ch in text.chars() {
        let ch_is_word = ch.is_alphanumeric();
        match is_word {
            None => {
                is_word = Some(ch_is_word);
                current.push(ch);
            }
            Some(w) if w == ch_is_word => {
                current.push(ch);
            }
            Some(_) => {
                tokens.push((std::mem::take(&mut current), is_word.unwrap()));
                current.push(ch);
                is_word = Some(ch_is_word);
            }
        }
    }
    if !current.is_empty() {
        tokens.push((current, is_word.unwrap_or(false)));
    }

    let mut out_tokens: Vec<(String, bool)> = Vec::new();
    let mut last_word_norm: Option<String> = None;

    for (token_text, token_is_word) in tokens {
        if token_is_word {
            let norm: String = token_text.chars().map(normalize_char).collect();
            if let Some(prev) = &last_word_norm
                && prev == &norm
            {
                // Accidental immediate repetition! Drop the separator preceding it
                if let Some((_, false)) = out_tokens.last() {
                    out_tokens.pop();
                }
                continue;
            }
            last_word_norm = Some(norm);
            out_tokens.push((token_text, true));
        } else {
            out_tokens.push((token_text, false));
        }
    }

    // Assemble and normalize redundant punctuation/spacing
    let mut result = String::with_capacity(text.len());
    let mut prev_char: Option<char> = None;

    for (t, _) in out_tokens {
        for ch in t.chars() {
            // Avoid duplicate punctuation like "..", ",,", "??"
            if matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
                && prev_char == Some(ch)
            {
                continue;
            }
            // Avoid space immediately before punctuation: "word ." -> "word."
            if matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
                && prev_char == Some(' ')
            {
                result.pop();
            }
            result.push(ch);
            prev_char = Some(ch);
        }
    }

    result
}

pub fn sanitize_plain_text(text: &str) -> String {
    let deduplicated = clean_repeated_words(text);
    let mut out = String::with_capacity(deduplicated.len());
    let mut prev_space = false;
    for ch in deduplicated.chars() {
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
    #[test]
    fn cleans_accidental_speech_repetitions_and_duplicate_words() {
        let input = "Genial. Genial, funciona muy bien. Está Está perfecto, así que bueno, esto es una transcripción de prueba para que veas cómo está transcribiendo.";
        let cleaned = super::clean_repeated_words(input);
        assert_eq!(
            cleaned,
            "Genial, funciona muy bien. Está perfecto, así que bueno, esto es una transcripción de prueba para que veas cómo está transcribiendo."
        );
        assert_eq!(
            super::clean_repeated_words("Hola, hola mundo."),
            "Hola mundo."
        );
        assert_eq!(
            super::clean_repeated_words("vamos vamos a ver"),
            "vamos a ver"
        );
        assert_eq!(
            super::clean_repeated_words("Esto es una prueba normal."),
            "Esto es una prueba normal."
        );
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
