mod paint;
mod settings;
mod shortcut;
mod ui;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use hypr_dictate::{Phase, valid_address, wav};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub enum Event {
    Command(String),
    Finished(u64, Result<(String, String)>),
    Catalog(String, u64, Result<Vec<String>>),
    Save(settings::Settings),
    SavePosition(i32),
    SaveServer(String),
}
struct Capture {
    child: Child,
    reader: thread::JoinHandle<Result<Vec<u8>>>,
    errors: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}
pub struct App {
    phase: Phase,
    capture: Option<Capture>,
    last_request_id: u64,
    catalog_generation: u64,
    tx: mpsc::Sender<Event>,
    client: reqwest::blocking::Client,
    key: String,
    endpoint: String,
    settings: settings::Settings,
    level: std::sync::Arc<std::sync::atomic::AtomicU32>,
    error: String,
}
impl App {
    pub fn color(&self) -> [u8; 3] {
        match self.phase {
            Phase::Idle => [55, 170, 130],
            Phase::Recording(_) => [235, 65, 75],
            Phase::Processing(_) => [230, 170, 40],
            Phase::Error => [160, 75, 210],
        }
    }
    fn discard_capture(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            let _ = capture.child.kill();
            let _ = capture.child.wait();
            let _ = capture.reader.join();
            let _ = capture.errors.join();
        }
    }
    fn start(&mut self, from_shortcut: bool) -> Result<()> {
        if !matches!(self.phase, Phase::Idle | Phase::Error) {
            return Ok(());
        }
        ensure!(
            !self.settings.model.trim().is_empty(),
            "Selecciona y guarda un modelo del catálogo antes de grabar"
        );
        self.discard_capture();
        let address = active_address()?;
        ensure!(valid_address(&address), "No hay ventana enfocada válida");
        let mut command = Command::new("pw-record");
        command.args([
            "--raw",
            "--format=s16",
            "--rate=16000",
            "--channels=1",
            "--sample-count=4800000",
        ]);
        if !self.settings.microphone.is_empty() {
            command.arg(format!("--target={}", self.settings.microphone));
        }
        let mut child = command
            .arg("-")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("No se pudo iniciar pw-record")?;
        let pid = child.id();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let errors = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.take(65536).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        let level = self.level.clone();
        let reader = thread::spawn(move || {
            let mut stdout = stdout;
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 2048];
            loop {
                let n = stdout.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                let peak = chunk[..n]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|v| i16::from_le_bytes(*v).unsigned_abs() as u32)
                    .max()
                    .unwrap_or(0);
                level.store(peak, std::sync::atomic::Ordering::Relaxed);
                bytes.extend_from_slice(&chunk[..n]);
                ensure!(bytes.len() <= 9_600_000, "Límite de audio excedido");
            }
            level.store(0, std::sync::atomic::Ordering::Relaxed);
            Ok(bytes)
        });
        if from_shortcut {
            let shortcut = self.settings.shortcut.clone();
            let tx = self.tx.clone();
            thread::spawn(move || {
                for tick in 0..20000 {
                    thread::sleep(Duration::from_millis(15));
                    if unsafe { libc::kill(pid as i32, 0) } != 0 {
                        break;
                    }
                    match shortcut::shortcut_held(&shortcut) {
                        Some(false) if tick >= 4 => {
                            let _ = tx.send(Event::Command("stop".into()));
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
            });
        }
        self.phase.start(&address);
        self.capture = Some(Capture {
            child,
            reader,
            errors,
        });
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        let request_id = self
            .last_request_id
            .checked_add(1)
            .context("Límite de peticiones excedido")?;
        let Some(address) = self.phase.stop(request_id) else {
            return Ok(());
        };
        self.last_request_id = request_id;
        let Some(mut capture) = self.capture.take() else {
            bail!("No hay captura");
        };
        // SIGINT lets PipeWire flush the last samples before closing stdout.
        unsafe {
            libc::kill(capture.child.id() as i32, libc::SIGINT);
        }
        let tx = self.tx.clone();
        let client = self.client.clone();
        let key = self.key.clone();
        let endpoint = self.endpoint.clone();
        let model = self.settings.model.clone();
        let temperature = self.settings.temperature;
        let prompt = self.settings.prompt.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(String, String)> {
                let status = capture.child.wait()?;
                let pcm = capture
                    .reader
                    .join()
                    .map_err(|_| anyhow::anyhow!("Falló el lector de audio"))??;
                let errors = capture
                    .errors
                    .join()
                    .map_err(|_| anyhow::anyhow!("Falló el lector de errores"))??;
                ensure!(
                    hypr_dictate::capture_valid(
                        status.code(),
                        std::os::unix::process::ExitStatusExt::signal(&status),
                        &errors,
                        &pcm
                    ),
                    "Captura inválida ({status}, {} bytes de audio): {}",
                    pcm.len(),
                    String::from_utf8_lossy(&errors)
                );
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
                                    "data": base64::engine::general_purpose::STANDARD.encode(wav(&pcm)),
                                    "format": "wav"
                                }
                            }
                        ]
                    }]
                });
                let request = transcription_request(&client, &endpoint, &key, &body);
                let bytes = send_transcription(request)?;
                let response: Value =
                    serde_json::from_slice(&bytes).context("Respuesta JSON inválida")?;
                ensure!(
                    response["choices"][0]["finish_reason"] == "stop",
                    "Respuesta incompleta; no se insertó texto"
                );
                let text = response["choices"][0]["message"]["content"]
                    .as_str()
                    .context("API sin transcripción")?
                    .trim()
                    .to_owned();
                ensure!(text.len() <= 100_000, "Transcripción demasiado grande");
                Ok((address, text))
            })();
            let _ = tx.send(Event::Finished(request_id, result));
        });
        Ok(())
    }
    fn save_server(
        &mut self,
        base_url: &str,
        persist: impl FnOnce(&settings::Settings) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            !matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)),
            "Espera a terminar el dictado"
        );
        let (endpoint, _) = api_endpoints(Some(base_url))?;
        let mut settings = self.settings.clone();
        settings.base_url = base_url.into();
        persist(&settings)?;
        self.settings = settings;
        self.endpoint = endpoint;
        Ok(())
    }
    fn request_catalog(&mut self) {
        self.catalog_generation += 1;
        let generation = self.catalog_generation;
        let base_url = self.settings.base_url.clone();
        let client = self.client.clone();
        let key = self.key.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = api_endpoints(Some(&base_url))
                .and_then(|(_, endpoint)| fetch_catalog(&client, &endpoint, &key));
            let _ = tx.send(Event::Catalog(base_url, generation, result));
        });
    }
    fn catalog_is_current(&self, base_url: &str, generation: u64) -> bool {
        self.settings.base_url == base_url && self.catalog_generation == generation
    }
    fn save_position(
        &mut self,
        x: i32,
        persist: impl FnOnce(&settings::Settings) -> Result<()>,
    ) -> Result<()> {
        let mut settings = self.settings.clone();
        settings.x = x;
        persist(&settings)?;
        self.settings = settings;
        Ok(())
    }
    fn finish(
        &mut self,
        request_id: u64,
        result: Result<(String, String)>,
        insert: impl FnOnce(&str, &str) -> Result<()>,
    ) -> Result<()> {
        if self.phase != Phase::Processing(request_id) {
            return Ok(());
        }
        result.and_then(|(address, text)| {
            if !text.is_empty() {
                insert(&address, &text)?;
            }
            self.phase = Phase::Idle;
            Ok(())
        })
    }
    fn report_error(&mut self, error: anyhow::Error, dictation_error: bool) {
        eprintln!("hypr-dictate: {error}");
        self.error = error.to_string();
        if self.capture.is_none()
            && (dictation_error
                || !matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)))
        {
            self.phase = Phase::Error;
        }
    }
    pub fn event(&mut self, event: Event, ui: &mut ui::Ui) {
        let dictation_error = matches!(&event, Event::Command(_) | Event::Finished(..));
        let result = match event {
            Event::Command(command) => match command.as_str() {
                "reload" | "reset" => {
                    self.discard_capture();
                    self.phase = Phase::Idle;
                    self.error.clear();
                    ui.level = 0.;
                    ui.message = "Dock reiniciado".into();
                    ui.dirty = true;
                    Ok(())
                }
                "start" => {
                    if matches!(self.phase, Phase::Processing(_)) {
                        self.phase = Phase::Idle;
                        ui.level = 0.;
                        ui.message = "Cancelado".into();
                        ui.dirty = true;
                        Ok(())
                    } else if matches!(self.phase, Phase::Recording(_)) {
                        self.stop()
                    } else {
                        self.start(true)
                    }
                }
                "stop" => self.stop(),
                "toggle" => {
                    if matches!(self.phase, Phase::Processing(_)) {
                        self.phase = Phase::Idle;
                        ui.level = 0.;
                        ui.message = "Cancelado".into();
                        ui.dirty = true;
                        Ok(())
                    } else if matches!(self.phase, Phase::Recording(_)) {
                        self.stop()
                    } else {
                        self.start(false)
                    }
                }
                _ => Ok(()),
            },
            Event::Catalog(base_url, generation, result) => {
                if !self.catalog_is_current(&base_url, generation) {
                    return;
                }
                match result {
                    Ok(models) => {
                        ui.models = models;
                        ui.catalog_loaded();
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            Event::SaveServer(base_url) => self
                .save_server(&base_url, settings::Settings::save)
                .map(|()| {
                    ui.server_saved(&self.settings.base_url);
                    self.request_catalog();
                }),
            Event::SavePosition(x) => self.save_position(x, settings::Settings::save).map(|()| {
                ui.position_saved(self.settings.x);
            }),
            Event::Save(settings) => (|| -> Result<()> {
                ensure!(
                    !matches!(self.phase, Phase::Recording(_) | Phase::Processing(_)),
                    "Espera a terminar el dictado"
                );
                ensure!(
                    ui.models.contains(&settings.model),
                    "Selecciona un modelo del catálogo"
                );
                ensure!(
                    settings.microphone.is_empty()
                        || ui.microphones.iter().any(|m| m.name == settings.microphone),
                    "Selecciona un micrófono detectado"
                );
                ensure!(
                    settings.monitor.is_empty()
                        || ui.monitors.iter().any(|m| m.name == settings.monitor),
                    "Selecciona una pantalla detectada"
                );
                if settings.shortcut != self.settings.shortcut {
                    shortcut::apply(&settings.shortcut, &self.settings.shortcut)?;
                }
                let mut settings = settings;
                settings.base_url = self.settings.base_url.clone();
                settings.save()?;
                self.settings = settings;
                ui.settings_saved(&self.settings);
                ui.message = "Ajustes guardados".into();
                Ok(())
            })(),
            Event::Finished(request_id, result) => {
                self.finish(request_id, result, |address, text| {
                    restore(address)?;
                    ui.type_text(text, address)
                })
            }
        };
        if let Err(error) = result {
            self.report_error(error, dictation_error);
            ui.message = self.error.clone();
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.discard_capture();
    }
}
fn api_endpoints(base: Option<&str>) -> Result<(String, String)> {
    let base = base.unwrap_or(settings::DEFAULT_BASE_URL);
    let mut url = reqwest::Url::parse(base)
        .map_err(|_| anyhow::anyhow!("La URL del servidor no es válida"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "La URL del servidor debe usar HTTP o HTTPS, sin credenciales, parámetros ni fragmentos"
    );
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{path}/v1/chat/completions"));
    let transcription = url.to_string();
    url.set_path(&format!("{path}/v1/models"));
    url.set_query(Some("client_version=1"));
    Ok((transcription, url.into()))
}
fn fetch_catalog(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    key: &str,
) -> Result<Vec<String>> {
    let response = client
        .get(endpoint)
        .bearer_auth(key)
        .send()?
        .error_for_status()?;
    let value: Value = response.json()?;
    let models = settings::routes(&value);
    ensure!(!models.is_empty(), "Catálogo de modelos vacío");
    Ok(models)
}
fn transcription_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    key: &str,
    body: &Value,
) -> reqwest::blocking::RequestBuilder {
    client
        .post(endpoint)
        .bearer_auth(key)
        .json(body)
        .timeout(Duration::from_secs(120))
}
fn transcription_error(error: anyhow::Error) -> anyhow::Error {
    let timed_out = error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
            || cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                error.kind() == std::io::ErrorKind::TimedOut
                    || error
                        .get_ref()
                        .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
                        .is_some_and(reqwest::Error::is_timeout)
            })
    });
    error.context(if timed_out {
        "Tiempo de espera agotado durante la transcripción"
    } else {
        "Falló la comunicación con la API durante la transcripción"
    })
}
fn send_transcription(request: reqwest::blocking::RequestBuilder) -> Result<Vec<u8>> {
    let response = request
        .send()
        .map_err(|error| transcription_error(error.into()))?;
    ensure!(
        response.status().is_success(),
        "API devolvió HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    response
        .take(2_000_001)
        .read_to_end(&mut bytes)
        .map_err(|error| transcription_error(error.into()))?;
    ensure!(bytes.len() <= 2_000_000, "Respuesta demasiado grande");
    Ok(bytes)
}
fn hypr(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("hyprctl").args(args).output()?;
    ensure!(
        output.status.success(),
        "Falló hyprctl {:?} ({}): {} {}",
        args,
        output.status,
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}
fn active_address() -> Result<String> {
    let value: Value = serde_json::from_slice(&hypr(&["-j", "activewindow"])?)?;
    Ok(value["address"].as_str().unwrap_or("").into())
}
fn restore(address: &str) -> Result<()> {
    ensure!(valid_address(address), "Destino inválido");
    if active_address()? == address {
        return Ok(());
    }
    let clients: Value = serde_json::from_slice(&hypr(&["-j", "clients"])?)?;
    ensure!(
        clients
            .as_array()
            .is_some_and(|clients| clients.iter().any(|c| c["address"] == address)),
        "La ventana original ya no existe; no se insertó texto"
    );
    hypr(&["dispatch", &hypr_dictate::focus_dispatch(address)])?;
    for _ in 0..20 {
        if active_address()? == address {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    bail!("No se pudo restaurar el foco; no se insertó texto")
}
fn socket_path() -> Result<PathBuf> {
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("Falta XDG_RUNTIME_DIR")?);
    let metadata = std::fs::symlink_metadata(&runtime)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Directorio runtime inválido"
    );
    Ok(runtime.join("hypr-dictate.sock"))
}
fn main() -> Result<()> {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let path = socket_path()?;
    if command != "run" {
        ensure!(
            ["start", "stop", "toggle", "reload", "reset"].contains(&command.as_str()),
            "Uso: hypr-dictate [run|start|stop|toggle|reload|reset]"
        );
        let mut stream = UnixStream::connect(path).context("Inicia primero hypr-dictate run")?;
        stream.write_all(command.as_bytes())?;
        return Ok(());
    }
    let key = std::env::var("CLIPROXYAPI_API_KEY").context("Falta CLIPROXYAPI_API_KEY")?;
    ensure!(!key.is_empty(), "CLIPROXYAPI_API_KEY está vacía");
    let settings = settings::Settings::load()?;
    let (endpoint, _) = api_endpoints(Some(&settings.base_url))?;
    // Never unlink an existing path: it may belong to another live process.
    let listener = UnixListener::bind(&path).context(
        "Socket ocupado: si no hay otro proceso, elimina manualmente el socket residual",
    )?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(path);
    let (tx, rx) = mpsc::channel();
    let sender = tx.clone();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let mut bytes = Vec::new();
            if (&mut stream).take(16).read_to_end(&mut bytes).is_ok()
                && let Ok(command) = String::from_utf8(bytes)
            {
                let _ = sender.send(Event::Command(command));
            }
        }
    });
    let mut app = App {
        phase: Phase::Idle,
        capture: None,
        last_request_id: 0,
        catalog_generation: 0,
        tx,
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        key,
        endpoint,
        settings,
        level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        error: String::new(),
    };
    app.request_catalog();
    ui::run(app, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn app(phase: Phase) -> App {
        let (tx, _) = mpsc::channel();
        App {
            phase,
            capture: None,
            last_request_id: 2,
            catalog_generation: 0,
            tx,
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
            key: String::new(),
            endpoint: String::new(),
            settings: settings::Settings::default(),
            level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            error: String::new(),
        }
    }

    #[test]
    fn cancelled_or_stale_results_never_insert_or_change_phase() {
        for phase in [
            Phase::Idle,
            Phase::Error,
            Phase::Recording("0xdef".into()),
            Phase::Processing(2),
        ] {
            let mut app = app(phase);
            let expected = format!("{:?}", app.phase);
            app.finish(1, Ok(("0xabc".into(), "cancelled text".into())), |_, _| {
                panic!("stale result must not restore focus or type text")
            })
            .unwrap();
            assert_eq!(format!("{:?}", app.phase), expected);
        }
    }

    #[test]
    fn cancelled_or_stale_errors_are_ignored() {
        for phase in [
            Phase::Idle,
            Phase::Error,
            Phase::Recording("0xdef".into()),
            Phase::Processing(2),
        ] {
            let mut app = app(phase);
            let expected = format!("{:?}", app.phase);
            app.finish(1, Err(anyhow::anyhow!("old request failed")), |_, _| {
                panic!("failed request must not type text")
            })
            .unwrap();
            assert_eq!(format!("{:?}", app.phase), expected);
            assert!(app.error.is_empty());
        }
    }

    #[test]
    fn saving_server_works_without_catalog_and_preserves_other_settings() {
        let mut app = app(Phase::Idle);
        app.settings.model = "saved/model".into();
        app.settings.shortcut = "SUPER + F9".into();
        let mut expected = app.settings.clone();
        expected.base_url = "https://proxy.example/service".into();
        let mut calls = 0;
        app.save_server(&expected.base_url, |candidate| {
            calls += 1;
            assert_eq!(format!("{candidate:?}"), format!("{expected:?}"));
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(format!("{:?}", app.settings), format!("{expected:?}"));
        assert_eq!(
            app.endpoint,
            "https://proxy.example/service/v1/chat/completions"
        );
    }

    #[test]
    fn server_save_failures_or_active_dictation_do_not_apply_changes() {
        for phase in [Phase::Recording("0xabc".into()), Phase::Processing(1)] {
            let mut app = app(phase);
            let original = format!("{:?}", app.settings);
            let error = app
                .save_server("http://127.0.0.1:9000", |_| {
                    panic!("must not persist server during dictation")
                })
                .unwrap_err();
            assert!(error.to_string().contains("Espera a terminar"));
            assert_eq!(format!("{:?}", app.settings), original);
        }
        let mut app = app(Phase::Idle);
        let original = format!("{:?}", app.settings);
        app.save_server("http://127.0.0.1:9000", |_| bail!("disk full"))
            .unwrap_err();
        assert_eq!(format!("{:?}", app.settings), original);
        assert!(app.endpoint.is_empty());
        app.save_server("ftp://invalid", |_| panic!("invalid URL must not persist"))
            .unwrap_err();
    }

    #[test]
    fn catalogue_replies_must_match_server_and_generation() {
        let mut app = app(Phase::Idle);
        app.settings.base_url = "http://127.0.0.1:9000".into();
        app.catalog_generation = 3;
        assert!(app.catalog_is_current("http://127.0.0.1:9000", 3));
        assert!(!app.catalog_is_current("http://127.0.0.1:8317", 3));
        assert!(!app.catalog_is_current("http://127.0.0.1:9000", 1));
    }

    #[test]
    fn api_urls_use_default_or_configured_http_and_https_base() {
        for (base, root) in [
            (None, "http://127.0.0.1:8317"),
            (Some("http://127.0.0.1:9000/"), "http://127.0.0.1:9000"),
            (Some("https://proxy.example"), "https://proxy.example"),
            (
                Some("https://proxy.example/service/"),
                "https://proxy.example/service",
            ),
        ] {
            let (transcription, catalog) = api_endpoints(base).unwrap();
            assert_eq!(transcription, format!("{root}/v1/chat/completions"));
            assert_eq!(catalog, format!("{root}/v1/models?client_version=1"));
        }
    }

    #[test]
    fn configured_server_receives_catalog_and_transcription_requests() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/proxy/", listener.local_addr().unwrap());
        let (transcription, _) = api_endpoints(Some(&base)).unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [r#"{"data":[{"id":"available/model"}]}"#, "{}"] {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                let length = request
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    })
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                socket.read_exact(&mut vec![0; length]).unwrap();
                requests.push(request.lines().next().unwrap().to_owned());
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let mut app = app(Phase::Idle);
        app.client = client.clone();
        app.tx = tx;
        app.settings.base_url = base.clone();
        app.request_catalog();
        let Event::Catalog(reply_base, generation, models) =
            rx.recv_timeout(Duration::from_secs(3)).unwrap()
        else {
            panic!("expected catalogue reply");
        };
        assert_eq!(reply_base, base);
        assert_eq!(generation, 1);
        assert!(app.catalog_is_current(&reply_base, generation));
        let models = models.unwrap();
        let body = send_transcription(transcription_request(
            &client,
            &transcription,
            "test",
            &json!({}),
        ))
        .unwrap();
        assert_eq!(models, ["available/model"]);
        assert_eq!(body, b"{}");
        assert_eq!(
            server.join().unwrap(),
            [
                "GET /proxy/v1/models?client_version=1 HTTP/1.1",
                "POST /proxy/v1/chat/completions HTTP/1.1",
            ]
        );
    }

    #[test]
    fn invalid_or_ambiguous_api_base_is_rejected_without_echoing_it() {
        for base in [
            "",
            "not-a-url",
            "ftp://proxy.example",
            "file:///tmp/api",
            "http://",
            "https://user:secret@proxy.example",
            "https://proxy.example?key=secret",
            "https://proxy.example#fragment",
        ] {
            let error = api_endpoints(Some(base)).unwrap_err();
            assert!(error.to_string().contains("URL del servidor"));
            assert!(!format!("{error:#}").contains("secret"));
        }
    }

    #[test]
    fn recording_without_a_saved_model_is_rejected_before_capture_or_focus_access() {
        let mut app = app(Phase::Idle);
        app.settings.model.clear();
        let error = app.start(false).unwrap_err();
        assert!(
            error.to_string().contains("Selecciona y guarda un modelo"),
            "{error:#}"
        );
        assert_eq!(app.phase, Phase::Idle);
        assert!(app.capture.is_none());
    }

    fn delayed_api(send_headers_first: bool, delay: Duration) -> (String, thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let length = String::from_utf8(request)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                })
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            socket.read_exact(&mut vec![0; length]).unwrap();
            let headers = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n";
            if send_headers_first {
                socket.write_all(headers).unwrap();
            }
            thread::sleep(delay);
            if !send_headers_first && socket.write_all(headers).is_err() {
                return;
            }
            let _ = socket.write_all(b"{}");
        });
        (endpoint, server)
    }

    #[test]
    fn transcription_uses_120_seconds_without_changing_catalog_timeout() {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let request = transcription_request(&client, "http://127.0.0.1/", "test", &json!({}))
            .build()
            .unwrap();
        assert_eq!(request.timeout(), Some(&Duration::from_secs(120)));
        let catalog = client.get("http://127.0.0.1/v1/models").build().unwrap();
        assert_eq!(
            catalog.timeout(),
            None,
            "catalog must retain the client default"
        );
    }

    #[test]
    fn transcription_can_outlive_the_client_default_timeout() {
        let (endpoint, server) = delayed_api(false, Duration::from_millis(150));
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let result = send_transcription(transcription_request(
            &client,
            &endpoint,
            "test",
            &json!({}),
        ));
        server.join().unwrap();
        assert_eq!(result.unwrap(), b"{}");
    }

    #[test]
    fn transcription_timeout_is_identified_before_headers_and_during_body() {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        for send_headers_first in [false, true] {
            let (endpoint, server) = delayed_api(send_headers_first, Duration::from_millis(200));
            let request = transcription_request(&client, &endpoint, "test", &json!({}))
                .timeout(Duration::from_millis(100));
            let error = send_transcription(request).unwrap_err();
            server.join().unwrap();
            assert!(
                error.to_string().contains("Tiempo de espera agotado"),
                "{error:#}"
            );
            assert!(!error.to_string().contains("conectar"));
        }
    }

    #[test]
    fn transcription_connection_failure_is_not_reported_as_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let error = send_transcription(transcription_request(
            &client,
            &endpoint,
            "test",
            &json!({}),
        ))
        .unwrap_err();
        assert!(error.to_string().contains("comunicación"), "{error:#}");
        assert!(!error.to_string().contains("Tiempo de espera agotado"));
    }

    #[test]
    fn current_result_inserts_once_and_finishes() {
        let mut app = app(Phase::Processing(2));
        let mut inserted = Vec::new();
        for _ in 0..2 {
            app.finish(
                2,
                Ok(("0xdef".into(), "current text".into())),
                |address, text| {
                    inserted.push((address.to_owned(), text.to_owned()));
                    Ok(())
                },
            )
            .unwrap();
        }
        assert_eq!(inserted, [("0xdef".into(), "current text".into())]);
        assert_eq!(app.phase, Phase::Idle);
    }

    #[test]
    fn old_completion_leaves_new_recording_stoppable_and_new_result_deliverable() {
        let mut app = app(Phase::Processing(1));
        app.phase = Phase::Idle;
        assert!(app.phase.start("0xdef"));
        app.finish(1, Ok(("0xabc".into(), "cancelled text".into())), |_, _| {
            panic!("cancelled request must not insert during the new recording")
        })
        .unwrap();
        assert_eq!(app.phase.stop(2), Some("0xdef".into()));
        app.finish(1, Err(anyhow::anyhow!("late failure")), |_, _| {
            panic!("old request must not insert during the new request")
        })
        .unwrap();
        assert_eq!(app.phase, Phase::Processing(2));
        app.finish(
            2,
            Ok(("0xdef".into(), "new text".into())),
            |address, text| {
                assert_eq!(address, "0xdef");
                assert_eq!(text, "new text");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(app.phase, Phase::Idle);
    }

    #[test]
    fn empty_current_result_finishes_without_inserting() {
        let mut app = app(Phase::Processing(2));
        app.finish(2, Ok(("0xdef".into(), String::new())), |_, _| {
            panic!("empty result must not restore focus or type text")
        })
        .unwrap();
        assert_eq!(app.phase, Phase::Idle);
    }

    #[test]
    fn current_request_and_insertion_errors_are_reported() {
        let mut app = app(Phase::Processing(2));
        let error = app
            .finish(2, Err(anyhow::anyhow!("API failed")), |_, _| Ok(()))
            .unwrap_err();
        app.report_error(error, true);
        assert_eq!(app.phase, Phase::Error);
        assert_eq!(app.error, "API failed");

        app.phase = Phase::Processing(3);
        let error = app
            .finish(3, Ok(("0xdef".into(), "text".into())), |_, _| {
                bail!("destination closed")
            })
            .unwrap_err();
        app.report_error(error, true);
        assert_eq!(app.phase, Phase::Error);
        assert_eq!(app.error, "destination closed");
    }

    #[test]
    fn unrelated_errors_preserve_recording_and_processing() {
        for phase in [Phase::Recording("0xdef".into()), Phase::Processing(2)] {
            for message in ["catalog failed", "Espera a terminar el dictado"] {
                let mut app = app(match &phase {
                    Phase::Recording(address) => Phase::Recording(address.clone()),
                    Phase::Processing(id) => Phase::Processing(*id),
                    _ => unreachable!(),
                });
                app.report_error(anyhow::anyhow!(message), false);
                assert_eq!(app.phase, phase);
                assert_eq!(app.error, message);
            }
        }
        let mut app = app(Phase::Idle);
        app.report_error(anyhow::anyhow!("catalog failed"), false);
        assert_eq!(app.phase, Phase::Error);
    }

    #[test]
    fn position_save_preserves_every_other_setting_and_active_phase() {
        for phase in [
            Phase::Idle,
            Phase::Recording("0xdef".into()),
            Phase::Processing(2),
        ] {
            let mut app = app(phase);
            app.settings = settings::Settings {
                model: "offline/model".into(),
                base_url: settings::DEFAULT_BASE_URL.into(),
                x: 123,
                shortcut: "SUPER + F9".into(),
                microphone: "saved-mic".into(),
                monitor: "saved-monitor".into(),
                temperature: 0.7,
                prompt: "saved prompt".into(),
            };
            let mut expected = app.settings.clone();
            expected.x = 456;
            let phase = format!("{:?}", app.phase);
            let mut calls = 0;
            app.save_position(456, |candidate| {
                calls += 1;
                assert_eq!(format!("{candidate:?}"), format!("{expected:?}"));
                Ok(())
            })
            .unwrap();
            assert_eq!(calls, 1);
            assert_eq!(format!("{:?}", app.settings), format!("{expected:?}"));
            assert_eq!(format!("{:?}", app.phase), phase);
        }
    }

    #[test]
    fn failed_position_save_preserves_committed_settings() {
        let mut app = app(Phase::Processing(2));
        let expected = format!("{:?}", app.settings);
        let error = app.save_position(456, |_| bail!("disk full")).unwrap_err();
        app.report_error(error, false);
        assert_eq!(format!("{:?}", app.settings), expected);
        assert_eq!(app.phase, Phase::Processing(2));
        assert_eq!(app.error, "disk full");
    }

    #[test]
    fn discarding_capture_kills_and_reaps_child() {
        let mut app = app(Phase::Idle);
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        app.capture = Some(Capture {
            child,
            reader: thread::spawn(|| Ok(Vec::new())),
            errors: thread::spawn(|| Ok(Vec::new())),
        });
        app.discard_capture();
        assert!(app.capture.is_none(), "residual capture must be removed");
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            -1,
            "capture is still alive or a zombie"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
