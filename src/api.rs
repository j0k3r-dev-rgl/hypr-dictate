use anyhow::{Context, Result, ensure};
use base64::Engine;
use crate::wav;
use serde_json::{Value, json};
use std::time::Duration;

pub fn api_endpoints(base: Option<&str>) -> Result<(String, String)> {
    let base = base.unwrap_or(crate::settings::DEFAULT_BASE_URL);
    let trimmed = base.trim().trim_end_matches('/');
    let clean = if trimmed.ends_with("/v1") {
        trimmed.strip_suffix("/v1").unwrap_or(trimmed).trim_end_matches('/')
    } else {
        trimmed
    };

    let mut url = reqwest::Url::parse(clean)
        .map_err(|_| anyhow::anyhow!("La URL del servidor no es válida"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "La URL del servidor debe usar HTTP o HTTPS, sin credenciales ni parámetros"
    );

    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{path}/v1/chat/completions"));
    let transcription = url.to_string();
    url.set_path(&format!("{path}/v1/models"));
    Ok((transcription, url.into()))
}

fn extract_api_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<Value>(body)
        && let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message").or_else(|| e.as_str().map(|_| e)))
            .and_then(Value::as_str)
    {
        return format!("HTTP {status}: {msg}");
    }
    let snippet: String = body.trim().chars().take(160).collect();
    if snippet.is_empty() {
        format!("La API devolvió código HTTP {status}")
    } else {
        format!("HTTP {status}: {snippet}")
    }
}

pub fn fetch_catalog(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    key: &str,
) -> Result<Vec<String>> {
    let mut req = client.get(endpoint);
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let response = req.send()?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        anyhow::bail!("{}", extract_api_error(status, &body));
    }
    let value: Value = response.json()?;
    let models = crate::settings::routes(&value);
    ensure!(!models.is_empty(), "Catálogo de modelos vacío");
    Ok(models)
}

pub fn send_transcription(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    key: &str,
    model: &str,
    temperature: f64,
    prompt: &str,
    pcm: &[u8],
) -> Result<String> {
    let wav_data = wav(pcm);
    let b64_audio = base64::engine::general_purpose::STANDARD.encode(&wav_data);

    let body = json!({
        "model": model,
        "stream": false,
        "temperature": temperature,
        "max_tokens": 8192,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": prompt
                },
                {
                    "type": "input_audio",
                    "input_audio": {
                        "data": b64_audio,
                        "format": "wav"
                    }
                }
            ]
        }]
    });

    let mut req = client
        .post(endpoint)
        .json(&body)
        .timeout(Duration::from_secs(120));

    if !key.is_empty() {
        req = req.bearer_auth(key);
    }

    let response = req.send().map_err(|e| {
        anyhow::anyhow!("Error comunicando con la API: {e}")
    })?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response.text().unwrap_or_default();
        anyhow::bail!("{}", extract_api_error(status, &err_body));
    }

    let val: Value = response.json().context("Respuesta JSON inválida")?;

    if let Some(err) = val.get("error").and_then(|e| e.get("message")).and_then(Value::as_str) {
        anyhow::bail!("Error de API: {err}");
    }

    let content = &val["choices"][0]["message"]["content"];
    let text = if let Some(s) = content.as_str() {
        s.trim().to_owned()
    } else if let Some(parts) = content.as_array() {
        parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_owned()
    } else {
        anyhow::bail!("La API no devolvió transcripción en choices[0].message.content");
    };

    ensure!(text.len() <= 100_000, "Transcripción demasiado grande");
    Ok(text)
}
