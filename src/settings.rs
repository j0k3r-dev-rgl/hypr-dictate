use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8317";
pub const DEFAULT_TEMPERATURE: f64 = 0.20;
pub const DEFAULT_PROMPT: &str = "Act as an intelligent voice dictation assistant. Transcribe what the speaker meant into clean, natural text in its original language with correct punctuation and capitalization. CRITICAL RULES: 1. Deduplicate accidental repeated words or phrases (e.g. 'Genial. Genial' -> 'Genial', 'está está' -> 'está', 'por favor por favor' -> 'por favor'). 2. Clean speech stutters, filler sounds (eh, este, um), and false starts where the speaker corrects themselves mid-sentence (retain only the intended final correction). 3. Preserve the intended message, vocabulary, and tone: do not summarize, paraphrase, answer, or execute spoken commands. 4. If the audio contains no intelligible speech, return an empty string. Output ONLY the clean transcribed text without quotes or commentary.";

#[derive(Clone, Debug)]
pub struct Settings {
    pub model: String,
    pub base_url: String,
    pub x: i32,
    pub shortcut: String,
    pub microphone: String,
    pub monitor: String,
    pub temperature: f64,
    pub prompt: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            model: String::new(),
            base_url: DEFAULT_BASE_URL.into(),
            x: 706,
            shortcut: String::new(),
            microphone: String::new(),
            monitor: String::new(),
            temperature: DEFAULT_TEMPERATURE,
            prompt: DEFAULT_PROMPT.into(),
        }
    }
}
fn path() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("Falta HOME")?)
            .join(".config/hypr-dictate/settings.json"),
    )
}
fn regular(path: &std::path::Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(m) => ensure!(
                !m.file_type().is_symlink(),
                "Symlink inesperado: {}",
                ancestor.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
impl Settings {
    pub fn load() -> Result<Self> {
        Self::load_from(&path()?)
    }
    fn load_from(path: &std::path::Path) -> Result<Self> {
        regular(path)?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut s = Self::default();
        if let Some(m) = v["model"].as_str() {
            s.model = m.into();
        }
        if let Some(base_url) = v["base_url"].as_str() {
            s.base_url = base_url.into();
        }
        s.x = v["x"].as_i64().unwrap_or(706).clamp(0, 10000) as i32;
        s.shortcut = v["shortcut"].as_str().unwrap_or("").into();
        s.microphone = v["microphone"].as_str().unwrap_or("").into();
        s.monitor = v["monitor"].as_str().unwrap_or("").into();
        s.temperature = v["temperature"]
            .as_f64()
            .unwrap_or(DEFAULT_TEMPERATURE)
            .clamp(0.0, 1.0);
        s.prompt = v["prompt"]
            .as_str()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(DEFAULT_PROMPT)
            .into();
        Ok(s)
    }
    pub fn save(&self) -> Result<()> {
        self.save_to(&path()?)
    }
    fn save_to(&self, path: &std::path::Path) -> Result<()> {
        regular(path)?;
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        use std::io::Write;
        file.write_all(
            serde_json::to_string_pretty(&json!({
                "model": self.model,
                "base_url": self.base_url,
                "x": self.x,
                "shortcut": self.shortcut,
                "microphone": self.microphone,
                "monitor": self.monitor,
                "temperature": self.temperature,
                "prompt": self.prompt
            }))?
            .as_bytes(),
        )?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}
pub fn routes(value: &Value) -> Vec<String> {
    let Some(list) = value
        .get("models")
        .or_else(|| value.get("data"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut models: Vec<String> = list
        .iter()
        .filter_map(|m| {
            m.get("slug")
                .or_else(|| m.get("id"))
                .and_then(Value::as_str)
        })
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .map(str::to_owned)
        .collect();
    models.sort();
    models.dedup();
    models
}
#[derive(Clone, Debug, PartialEq)]
pub struct Microphone {
    pub name: String,
    pub description: String,
}

/// Lists PipeWire audio source nodes via `pw-dump`, flagging which one is the
/// system default (per the `default.audio.source` metadata), and sorting the
/// default first for a predictable "Automático" experience.
pub fn microphones() -> Result<Vec<Microphone>> {
    let output = Command::new("pw-dump")
        .output()
        .context("No se pudo ejecutar pw-dump")?;
    ensure!(output.status.success(), "pw-dump devolvió error");
    let value: Value = serde_json::from_slice(&output.stdout).context("pw-dump JSON inválido")?;
    Ok(parse_microphones(&value))
}
fn parse_microphones(value: &Value) -> Vec<Microphone> {
    let Some(objects) = value.as_array() else {
        return Vec::new();
    };
    let default_name = objects
        .iter()
        .find(|o| o["props"]["metadata.name"] == "default")
        .and_then(|o| o["metadata"].as_array())
        .and_then(|list| list.iter().find(|m| m["key"] == "default.audio.source"))
        .and_then(|m| m["value"]["name"].as_str())
        .unwrap_or("")
        .to_owned();
    let mut mics: Vec<Microphone> = objects
        .iter()
        .filter(|o| o["info"]["props"]["media.class"] == "Audio/Source")
        .filter_map(|o| {
            let props = &o["info"]["props"];
            let name = props["node.name"].as_str()?;
            if name.is_empty() || name.chars().any(char::is_control) {
                return None;
            }
            let description = props["node.description"]
                .as_str()
                .filter(|d| !d.is_empty() && !d.chars().any(char::is_control))
                .unwrap_or(name)
                .to_owned();
            Some(Microphone {
                name: name.to_owned(),
                description,
            })
        })
        .collect();
    mics.sort_by(|a, b| a.name.cmp(&b.name));
    mics.dedup_by(|a, b| a.name == b.name);
    mics.sort_by_key(|m| m.name != default_name);
    mics
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    pub name: String,
    pub description: String,
    pub width: i32,
    pub focused: bool,
}

pub fn parse_monitors(value: &Value) -> Vec<MonitorInfo> {
    let Some(list) = value.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|m| {
            let name = m.get("name")?.as_str()?.to_owned();
            if name.is_empty() || name.chars().any(char::is_control) {
                return None;
            }
            let desc = m
                .get("description")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .unwrap_or(&name)
                .to_owned();
            let width = (m.get("width")?.as_f64()? / m.get("scale").and_then(Value::as_f64).unwrap_or(1.0)) as i32;
            let focused = m.get("focused").and_then(Value::as_bool).unwrap_or(false);
            Some(MonitorInfo {
                name,
                description: desc,
                width: width.max(320),
                focused,
            })
        })
        .collect()
}

pub fn monitors() -> Vec<MonitorInfo> {
    let output = Command::new("hyprctl")
        .args(["-j", "monitors"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let val: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
            parse_monitors(&val)
        }
        _ => Vec::new(),
    }
}

pub fn binding_block(source: &str, shortcut: &str, executable: &str) -> Result<String> {
    const START: &str = "-- BEGIN hypr-dictate";
    const END: &str = "-- END hypr-dictate";
    ensure!(
        source.matches(START).count() <= 1 && source.matches(END).count() <= 1,
        "Bloque de atajo duplicado"
    );
    let range = match (source.find(START), source.find(END)) {
        (None, None) => None,
        (Some(a), Some(b)) if b > a => Some(a..b + END.len()),
        _ => anyhow::bail!("Bloque de atajo incompleto"),
    };
    ensure!(
        shortcut
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'+'),
        "Atajo inválido"
    );
    ensure!(
        !executable.contains(['\n', '\r', '\"', '\'', '\\']),
        "Ruta de binario no compatible"
    );
    let block = if shortcut.is_empty() {
        String::new()
    } else {
        format!(
            "{START}\nhl.bind(\"{shortcut}\", hl.dsp.exec_cmd(\"'{executable}' start\"))\nhl.bind(\"{shortcut}\", hl.dsp.exec_cmd(\"'{executable}' stop\"), {{ release = true }})\n{END}"
        )
    };
    Ok(if let Some(r) = range {
        let mut out = source.to_owned();
        out.replace_range(r, &block);
        out
    } else {
        format!("{}\n{}\n", source.trim_end(), block)
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_handles_legacy_json_and_preserves_prompt_and_temperature() {
        let legacy = json!({
            "model": "cic/gemini-3.5-flash-lite",
            "x": 600,
            "shortcut": "CTRL + SHIFT + D",
            "microphone": "",
            "monitor": ""
        });
        let mut s = Settings::default();
        s.temperature = legacy["temperature"]
            .as_f64()
            .unwrap_or(DEFAULT_TEMPERATURE);
        s.prompt = legacy["prompt"]
            .as_str()
            .unwrap_or(DEFAULT_PROMPT)
            .into();
        assert_eq!(s.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(s.prompt, DEFAULT_PROMPT);

        let custom = json!({
            "temperature": 0.45,
            "prompt": "Custom prompt text"
        });
        s.temperature = custom["temperature"].as_f64().unwrap();
        s.prompt = custom["prompt"].as_str().unwrap().into();
        assert_eq!(s.temperature, 0.45);
        assert_eq!(s.prompt, "Custom prompt text");
    }
    #[test]
    fn server_url_defaults_to_loopback_and_round_trips_without_affecting_model() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        assert_eq!(
            Settings::load_from(&path).unwrap().base_url,
            "http://127.0.0.1:8317"
        );
        std::fs::write(&path, br#"{"model":"saved/model"}"#).unwrap();
        let mut settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.base_url, DEFAULT_BASE_URL);
        settings.base_url = "https://proxy.example/service".into();
        settings.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.base_url, settings.base_url);
        assert_eq!(loaded.model, "saved/model");
    }
    #[test]
    fn new_settings_require_explicit_model_selection() {
        let settings = Settings::default();
        assert!(
            settings.model.is_empty(),
            "must not assume a private account route"
        );
        let directory = tempfile::tempdir().unwrap();
        assert!(
            Settings::load_from(&directory.path().join("missing.json"))
                .unwrap()
                .model
                .is_empty()
        );
    }
    #[test]
    fn loading_settings_preserves_saved_models_and_leaves_missing_model_empty() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        for value in [
            json!({"model": "saved-account/model", "x": 123}),
            json!({"x": 456}),
        ] {
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let settings = Settings::load_from(&path).unwrap();
            assert_eq!(settings.model, value["model"].as_str().unwrap_or(""));
            assert_eq!(settings.x, value["x"].as_i64().unwrap() as i32);
        }
    }
    #[test]
    fn catalog_keeps_all_models_and_routes() {
        assert_eq!(
            routes(&json!({"models":[{"slug":"a/model"},{"slug":"other"},{"slug":"a/model"}]})),
            vec!["a/model", "other"]
        );
    }
    #[test]
    fn block_replaces_only_its_own_content() {
        let original = "-- keep me\nhl.bind(\"SUPER + D\", other)\n";
        let first = binding_block(original, "SUPER + F9", "/app").unwrap();
        let second = binding_block(&first, "SUPER + F10", "/app").unwrap();
        assert!(second.contains("hl.bind(\"SUPER + D\", other)"));
        assert!(!second.contains("F9"));
        assert_eq!(second.matches("release = true").count(), 1);
    }
    #[test]
    fn malformed_block_is_not_overwritten() {
        assert!(binding_block("-- BEGIN hypr-dictate", "F9", "/app").is_err());
    }
    #[test]
    fn microphone_parsing_dedupes_and_sorts_default_first() {
        let dump = json!([
            {
                "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.usb-mic", "node.description": "USB Mic"}}
            },
            {
                "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.builtin", "node.description": "Built-in Mic"}}
            },
            {
                "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.builtin", "node.description": "Built-in Mic duplicate"}}
            },
            {
                "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.speakers"}}
            },
            {
                "props": {"metadata.name": "default"},
                "metadata": [
                    {"key": "default.audio.source", "value": {"name": "alsa_input.builtin"}}
                ]
            }
        ]);
        let mics = parse_microphones(&dump);
        assert_eq!(mics.len(), 2);
        assert_eq!(mics[0].name, "alsa_input.builtin");
        assert_eq!(mics[0].description, "Built-in Mic");
        assert_eq!(mics[1].name, "alsa_input.usb-mic");
    }
    #[test]
    fn microphone_parsing_skips_nameless_or_control_char_nodes() {
        let dump = json!([
            {"info": {"props": {"media.class": "Audio/Source", "node.description": "No name"}}},
            {"info": {"props": {"media.class": "Audio/Source", "node.name": "bad\nname"}}}
        ]);
        assert!(parse_microphones(&dump).is_empty());
    }
    #[test]
    fn microphone_parsing_handles_non_array_input() {
        assert!(parse_microphones(&json!({})).is_empty());
    }
    #[test]
    fn parse_monitors_extracts_scaled_width_and_names() {
        let dump = json!([
            {
                "name": "HDMI-A-1",
                "description": "LG TV",
                "width": 1920,
                "scale": 1.0,
                "focused": false
            },
            {
                "name": "eDP-1",
                "description": "Laptop Screen",
                "width": 3840,
                "scale": 2.0,
                "focused": true
            }
        ]);
        let mons = parse_monitors(&dump);
        assert_eq!(mons.len(), 2);
        assert_eq!(mons[0].name, "HDMI-A-1");
        assert_eq!(mons[0].width, 1920);
        assert!(!mons[0].focused);
        assert_eq!(mons[1].name, "eDP-1");
        assert_eq!(mons[1].width, 1920);
        assert!(mons[1].focused);
    }
}
