use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8317";
pub const DEFAULT_TEMPERATURE: f64 = 0.20;
pub const DEFAULT_PROMPT: &str = "Act as an intelligent voice dictation assistant. Transcribe what the speaker meant into clean, natural text in its original language with correct punctuation and capitalization. CRITICAL RULES: 1. Deduplicate accidental repeated words or phrases (e.g. 'Genial. Genial' -> 'Genial', 'está está' -> 'está', 'por favor por favor' -> 'por favor'). 2. Clean speech stutters, filler sounds (eh, este, um), and false starts where the speaker corrects themselves mid-sentence (retain only the intended final correction). 3. Preserve the intended message, vocabulary, and tone: do not summarize, paraphrase, answer, or execute spoken commands. 4. If the audio contains no intelligible speech, return an empty string. Output ONLY the clean transcribed text without quotes or commentary.";

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub x: i32,
    pub y: i32,
    pub shortcut: String,
    pub microphone: String,
    pub temperature: f64,
    pub prompt: String,
    pub hold_to_talk: bool,
    pub autostart: bool,
    pub position: String,
}

pub const DOCK_POSITIONS: [&str; 4] = ["Abajo", "Arriba", "Izquierda", "Derecha"];

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: String::new(),
            base_url: DEFAULT_BASE_URL.into(),
            api_key: std::env::var("CLIPROXYAPI_API_KEY").unwrap_or_default(),
            x: 700,
            y: 950,
            shortcut: "F9".into(),
            microphone: String::new(),
            temperature: DEFAULT_TEMPERATURE,
            prompt: DEFAULT_PROMPT.into(),
            hold_to_talk: true,
            autostart: true,
            position: "Abajo".into(),
        }
    }
}

pub fn settings_path() -> Result<PathBuf> {
    let dir = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .context("Falta variable de entorno APPDATA o USERPROFILE")?
            .join("hypr-dictate")
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("Falta variable de entorno HOME")?
            .join(".config")
            .join("hypr-dictate")
    };
    Ok(dir.join("settings.json"))
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = settings_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read(&path)?;
        let v: Value = serde_json::from_slice(&data)?;
        let mut s = Self::default();

        if let Some(m) = v["model"].as_str() {
            s.model = m.into();
        }
        if let Some(base_url) = v["base_url"].as_str() {
            s.base_url = base_url.into();
        }
        if let Some(api_key) = v["api_key"].as_str()
            && !api_key.trim().is_empty()
        {
            s.api_key = api_key.trim().into();
        }
        if let Some(x) = v["x"].as_i64() {
            s.x = x as i32;
        }
        if let Some(y) = v["y"].as_i64() {
            s.y = y as i32;
        }
        if let Some(sc) = v["shortcut"].as_str() {
            s.shortcut = sc.into();
        }
        if let Some(mic) = v["microphone"].as_str() {
            s.microphone = mic.into();
        }
        if let Some(temp) = v["temperature"].as_f64() {
            s.temperature = temp.clamp(0.0, 1.0);
        }
        if let Some(prompt) = v["prompt"].as_str()
            && !prompt.trim().is_empty()
        {
            s.prompt = prompt.into();
        }
        if let Some(htt) = v["hold_to_talk"].as_bool() {
            s.hold_to_talk = htt;
        }
        if let Some(auto) = v["autostart"].as_bool() {
            s.autostart = auto;
        }
        if let Some(pos) = v["position"].as_str() {
            s.position = pos.into();
        }

        Ok(s)
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&json!({
            "model": self.model,
            "base_url": self.base_url,
            "api_key": self.api_key,
            "x": self.x,
            "y": self.y,
            "shortcut": self.shortcut,
            "microphone": self.microphone,
            "temperature": self.temperature,
            "prompt": self.prompt,
            "hold_to_talk": self.hold_to_talk,
            "autostart": self.autostart,
            "position": self.position
        }))?;
        std::fs::write(&path, json)?;
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
