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
    Catalog(Result<Vec<String>>),
    Save(settings::Settings),
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
                let response = client
                    .post(&endpoint)
                    .bearer_auth(key)
                    .json(&body)
                    .send()
                    .context("No se pudo conectar con la API")?;
                ensure!(
                    response.status().is_success(),
                    "API devolvió HTTP {}",
                    response.status()
                );
                let mut bytes = Vec::new();
                response.take(2_000_001).read_to_end(&mut bytes)?;
                ensure!(bytes.len() <= 2_000_000, "Respuesta demasiado grande");
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
            Event::Catalog(result) => match result {
                Ok(models) => {
                    ui.models = models;
                    ui.catalog_loaded();
                    Ok(())
                }
                Err(e) => Err(e),
            },
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
                settings.save()?;
                self.settings = settings;
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
    let app = App {
        phase: Phase::Idle,
        capture: None,
        last_request_id: 0,
        tx,
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        key,
        endpoint: "http://192.168.0.100:8317/v1/chat/completions".into(),
        settings: settings::Settings::load()?,
        level: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        error: String::new(),
    };
    let catalog_client = app.client.clone();
    let catalog_key = app.key.clone();
    let catalog_tx = app.tx.clone();
    thread::spawn(move || {
        let result = (|| -> Result<Vec<String>> {
            let response = catalog_client
                .get("http://192.168.0.100:8317/v1/models?client_version=1")
                .bearer_auth(catalog_key)
                .send()?
                .error_for_status()?;
            let value: Value = response.json()?;
            let models = settings::routes(&value);
            ensure!(!models.is_empty(), "Catálogo de modelos vacío");
            Ok(models)
        })();
        let _ = catalog_tx.send(Event::Catalog(result));
    });
    ui::run(app, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(phase: Phase) -> App {
        let (tx, _) = mpsc::channel();
        App {
            phase,
            capture: None,
            last_request_id: 2,
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
