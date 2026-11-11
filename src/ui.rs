use crate::{
    api,
    audio::{self, Microphone},
    input, paint,
    settings::{self, Settings},
    shortcut,
};
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use hypr_dictate::{DictationMode, Phase, capture_valid, sanitize_text};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU32, Ordering},
    mpsc::{Receiver, Sender, channel},
};
use std::thread;
use std::time::Duration;

#[derive(PartialEq, Clone, Copy)]
pub enum SettingsTab {
    Server,
    General,
}

pub enum UiEvent {
    ShortcutDown(DictationMode),
    ShortcutUp(DictationMode),
    Cancel,
    IpcCommand(String),
    CatalogLoaded(Result<Vec<String>, String>),
    TranscriptionFinished {
        request_id: u64,
        address: String,
        mode: DictationMode,
        result: Result<String, String>,
    },
}

pub struct DictateApp {
    pub settings: Settings,
    pub phase: Phase,
    pub level: Arc<AtomicU32>,
    pub capture: Option<audio::ActiveCapture>,
    pub request_id_counter: u64,
    pub models: Vec<String>,
    pub microphones: Vec<Microphone>,
    pub client: reqwest::blocking::Client,

    pub tx: Sender<UiEvent>,
    pub rx: Receiver<UiEvent>,
    pub shared_shortcuts: Arc<RwLock<shortcut::ShortcutSettings>>,
    pub press_start: Option<std::time::Instant>,

    // UI state
    pub show_settings: bool,
    pub active_tab: SettingsTab,
    pub show_api_key: bool,
    pub status_message: String,
    pub catalog_status: String,
    pub model_filter: String,

    pub anim_time: f64,
    pub open: f32,
    pub startup_frames: u8,
}

unsafe fn find_own_window() -> windows_sys::Win32::Foundation::HWND {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: isize) -> i32 {
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == GetCurrentProcessId() {
                let mut buf = [0u16; 64];
                let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                if len > 0 {
                    let title = String::from_utf16_lossy(&buf[..len as usize]);
                    if title == "hypr-dictate" {
                        *(lparam as *mut HWND) = hwnd;
                        return 0; // stop
                    }
                }
            }
            1
        }
    }

    unsafe {
        let mut found_hwnd: HWND = std::ptr::null_mut();
        EnumWindows(Some(enum_proc), &mut found_hwnd as *mut HWND as isize);
        found_hwnd
    }
}

impl DictateApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        initial_settings: Settings,
        shared_shortcuts: Arc<RwLock<shortcut::ShortcutSettings>>,
    ) -> Self {
        let (tx, rx) = channel();
        let level = Arc::new(AtomicU32::new(0));

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        let mics = audio::microphones();

        let mut app = Self {
            settings: initial_settings,
            phase: Phase::Idle,
            level,
            capture: None,
            request_id_counter: 0,
            models: Vec::new(),
            microphones: mics,
            client,
            tx,
            rx,
            shared_shortcuts,
            press_start: None,
            show_settings: false,
            active_tab: SettingsTab::Server,
            show_api_key: false,
            status_message: "Listo".into(),
            catalog_status: String::new(),
            model_filter: String::new(),
            anim_time: 0.0,
            open: 0.0,
            startup_frames: 0,
        };

        app.refresh_catalog();
        app
    }

    pub fn sync_shortcut(&self) {
        if let Ok(mut guard) = self.shared_shortcuts.write() {
            *guard = shortcut::ShortcutSettings {
                standard: self.settings.shortcut.clone(),
                formal: self.settings.shortcut_formal.clone(),
                hold_to_talk: self.settings.hold_to_talk,
            };
        }
    }

    pub fn refresh_catalog(&mut self) {
        self.catalog_status = "Cargando modelos...".into();
        let tx = self.tx.clone();
        let client = self.client.clone();
        let base_url = self.settings.base_url.clone();
        let key = self.settings.api_key.clone();

        thread::spawn(move || {
            let res = (|| -> anyhow::Result<Vec<String>> {
                let (_, endpoint) = api::api_endpoints(Some(&base_url))?;
                api::fetch_catalog(&client, &endpoint, &key)
            })();

            let event = match res {
                Ok(m) => UiEvent::CatalogLoaded(Ok(m)),
                Err(e) => UiEvent::CatalogLoaded(Err(e.to_string())),
            };
            let _ = tx.send(event);
        });
    }

    pub fn start_dictation(&mut self, mode: DictationMode) {
        if !matches!(self.phase, Phase::Idle | Phase::Error) {
            return;
        }

        if self.settings.model.trim().is_empty() {
            if let Some(first) = self.models.first() {
                self.settings.model = first.clone();
            } else {
                self.status_message = "Selecciona un modelo en Ajustes".into();
                self.phase = Phase::Error;
                return;
            }
        }

        let address = match input::active_window() {
            Ok(addr) => addr,
            Err(e) => {
                self.status_message = format!("Error: {e}");
                self.phase = Phase::Error;
                return;
            }
        };

        match audio::start_recording(&self.settings.microphone, self.level.clone()) {
            Ok(capture) => {
                self.capture = Some(capture);
                self.phase = Phase::Recording {
                    address,
                    mode,
                };
                self.status_message = match mode {
                    DictationMode::Transcription => "Grabando dictado...".into(),
                    DictationMode::Formal => "Grabando redacción formal...".into(),
                };
            }
            Err(e) => {
                self.status_message = format!("Error micrófono: {e}");
                self.phase = Phase::Error;
            }
        }
    }

    pub fn stop_dictation_with_mode(&mut self, target_mode: DictationMode) {
        self.request_id_counter += 1;
        let req_id = self.request_id_counter;

        let Some((address, _orig_mode)) = self.phase.stop(req_id) else {
            return;
        };

        let Some(capture) = self.capture.take() else {
            self.phase = Phase::Idle;
            return;
        };

        self.status_message = match target_mode {
            DictationMode::Transcription => "Transcribiendo...".into(),
            DictationMode::Formal => "Redactando formalmente...".into(),
        };

        let pcm = match capture.stop() {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status_message = format!("Error audio: {e}");
                self.phase = Phase::Error;
                return;
            }
        };

        if !capture_valid(&pcm) {
            self.status_message = "Audio demasiado corto".into();
            self.phase = Phase::Error;
            return;
        }

        let tx = self.tx.clone();
        let client = self.client.clone();
        let base_url = self.settings.base_url.clone();
        let key = self.settings.api_key.clone();
        let model = self.settings.model.clone();
        let temp = self.settings.temperature;
        let prompt = match target_mode {
            DictationMode::Transcription => self.settings.prompt.clone(),
            DictationMode::Formal => self.settings.prompt_formal.clone(),
        };

        thread::spawn(move || {
            let res = (|| -> anyhow::Result<String> {
                let (endpoint, _) = api::api_endpoints(Some(&base_url))?;
                api::send_transcription(&client, &endpoint, &key, &model, temp, &prompt, &pcm)
            })();

            let _ = tx.send(UiEvent::TranscriptionFinished {
                request_id: req_id,
                address,
                mode: target_mode,
                result: res.map_err(|e| e.to_string()),
            });
        });
    }

    pub fn stop_dictation(&mut self) {
        let current_mode = match &self.phase {
            Phase::Recording { mode, .. } => *mode,
            _ => DictationMode::Transcription,
        };
        self.stop_dictation_with_mode(current_mode);
    }

    pub fn cancel_dictation(&mut self) {
        if let Some(capture) = self.capture.take() {
            let _ = capture.stop();
        }
        self.press_start = None;
        self.phase = Phase::Idle;
        self.level.store(0, Ordering::Relaxed);
        self.status_message = "Cancelado".into();
    }

    pub fn toggle_dictation(&mut self, mode: DictationMode) {
        match &self.phase {
            Phase::Idle | Phase::Error => self.start_dictation(mode),
            Phase::Recording { mode: active_mode, .. } => {
                let m = *active_mode;
                self.stop_dictation_with_mode(m);
            }
            Phase::Processing { .. } => self.cancel_dictation(),
        }
    }

    pub fn process_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                UiEvent::ShortcutDown(mode) => {
                    match &self.phase {
                        Phase::Idle | Phase::Error => {
                            self.press_start = Some(std::time::Instant::now());
                            self.start_dictation(mode);
                        }
                        Phase::Recording { mode: active_mode, .. } => {
                            // If shortcut pressed again while recording, finish and transcribe (toggle behavior)
                            let target_mode = *active_mode;
                            self.press_start = None;
                            self.stop_dictation_with_mode(target_mode);
                        }
                        Phase::Processing { .. } => {
                            self.cancel_dictation();
                        }
                    }
                }
                UiEvent::ShortcutUp(mode) => {
                    if let Phase::Recording { mode: active_mode, .. } = &self.phase {
                        if *active_mode == mode {
                            if let Some(start) = self.press_start.take() {
                                // If held for at least 350ms, release completes recording (push-to-talk).
                                // If released quicker (<350ms), it's treated as a single tap toggle: keeps recording.
                                if start.elapsed() >= Duration::from_millis(350) {
                                    self.stop_dictation_with_mode(mode);
                                }
                            }
                        }
                    }
                }
                UiEvent::Cancel => {
                    if matches!(self.phase, Phase::Recording { .. } | Phase::Processing { .. }) {
                        self.cancel_dictation();
                    }
                }
                UiEvent::IpcCommand(cmd) => match cmd.as_str() {
                    "start" => {
                        if matches!(self.phase, Phase::Idle | Phase::Error) {
                            self.start_dictation(DictationMode::Transcription);
                        } else if matches!(self.phase, Phase::Processing { .. }) {
                            self.cancel_dictation();
                        } else if matches!(self.phase, Phase::Recording { .. }) {
                            self.stop_dictation();
                        }
                    }
                    "start-formal" | "formal" => {
                        if matches!(self.phase, Phase::Idle | Phase::Error) {
                            self.start_dictation(DictationMode::Formal);
                        } else if matches!(self.phase, Phase::Processing { .. }) {
                            self.cancel_dictation();
                        } else if matches!(self.phase, Phase::Recording { .. }) {
                            self.stop_dictation_with_mode(DictationMode::Formal);
                        }
                    }
                    "stop" => {
                        if matches!(self.phase, Phase::Recording { .. }) {
                            self.stop_dictation();
                        }
                    }
                    "cancel" => self.cancel_dictation(),
                    "toggle" => self.toggle_dictation(DictationMode::Transcription),
                    "toggle-formal" => self.toggle_dictation(DictationMode::Formal),
                    "reset" | "reload" => {
                        self.cancel_dictation();
                        self.status_message = "Reiniciado".into();
                    }
                    "settings" => {
                        self.show_settings = !self.show_settings;
                        self.startup_frames = 0;
                    }
                    _ => {}
                },
                UiEvent::CatalogLoaded(result) => match result {
                    Ok(models) => {
                        self.models = models;
                        self.catalog_status = format!("OK: {} modelos disponibles", self.models.len());
                        if self.settings.model.is_empty() && !self.models.is_empty() {
                            self.settings.model = self.models[0].clone();
                        }
                        if matches!(self.phase, Phase::Error) && self.status_message.contains("modelo") {
                            self.phase = Phase::Idle;
                            self.status_message = "Listo".into();
                        }
                    }
                    Err(err) => {
                        self.catalog_status = format!("Error al conectar: {err}");
                    }
                },
                UiEvent::TranscriptionFinished {
                    request_id,
                    address,
                    mode,
                    result,
                } => {
                    if self.phase != (Phase::Processing { request_id, mode }) {
                        return;
                    }

                    match result {
                        Ok(text) => {
                            let preserve_newlines = mode == DictationMode::Formal;
                            let clean = sanitize_text(&text, preserve_newlines);
                            if !clean.is_empty() {
                                let _ = input::restore_focus(&address);
                                thread::sleep(Duration::from_millis(60));
                                if let Err(e) = input::type_text(&clean) {
                                    self.status_message = format!("Error escritura: {e}");
                                    self.phase = Phase::Error;
                                    return;
                                }
                            }
                            self.phase = Phase::Idle;
                            self.status_message = "Listo".into();
                        }
                        Err(err) => {
                            self.status_message = format!("Error: {err}");
                            self.phase = Phase::Error;
                        }
                    }
                }
            }
        }
    }

    pub fn state_color(&self) -> Color32 {
        match self.phase {
            Phase::Idle => Color32::from_rgb(55, 170, 130),       // Emerald Teal
            Phase::Recording { mode: DictationMode::Transcription, .. } => Color32::from_rgb(235, 65, 75), // Ruby Red
            Phase::Recording { mode: DictationMode::Formal, .. } => Color32::from_rgb(75, 135, 245), // Sapphire Blue
            Phase::Processing { mode: DictationMode::Transcription, .. } => Color32::from_rgb(230, 170, 40), // Amber Gold
            Phase::Processing { mode: DictationMode::Formal, .. } => Color32::from_rgb(155, 85, 245), // Royal Amethyst
            Phase::Error => Color32::from_rgb(220, 60, 60),      // Error Red
        }
    }

    pub fn update_window_region(&self, ctx: &egui::Context) {
        let hwnd = unsafe { find_own_window() };
        if hwnd.is_null() {
            return;
        }
        unsafe {
            use windows_sys::Win32::Foundation::POINT;
            use windows_sys::Win32::Graphics::Gdi::{
                ALTERNATE, CreatePolygonRgn, CreateRoundRectRgn, SetWindowRgn,
            };
            let ppp = ctx.pixels_per_point();
            if self.show_settings {
                let w = (470.0 * ppp).round() as i32;
                let h = (560.0 * ppp).round() as i32;
                let r = (28.0 * ppp).round() as i32;
                let rgn = CreateRoundRectRgn(0, 0, w, h, r, r);
                if !rgn.is_null() {
                    SetWindowRgn(hwnd, rgn, 1);
                }
            } else {
                let eased = paint::ease(self.open as f64) as f32;
                let width = 176.0;
                let height = 44.0;
                let current_w = 88.0 + eased * 76.0 + 4.0;
                let current_h = 14.0 + eased * 24.0 + 2.0;
                let left = (width - current_w) / 2.0;

                let is_top = self.settings.position == "Arriba";
                let (flat_y, arch_y) = if is_top {
                    (1.0, current_h)
                } else {
                    (height, height - current_h)
                };

                let pts = paint::carapace_points(left, left + current_w, flat_y, arch_y);
                let gdi_pts: Vec<POINT> = pts
                    .into_iter()
                    .map(|p| POINT {
                        x: (p.x * ppp).round() as i32,
                        y: (p.y * ppp).round() as i32,
                    })
                    .collect();

                let rgn = CreatePolygonRgn(gdi_pts.as_ptr(), gdi_pts.len() as i32, ALTERNATE);
                if !rgn.is_null() {
                    SetWindowRgn(hwnd, rgn, 1);
                }
            }
        }
    }

    pub fn reposition_window(&self, ctx: &egui::Context) {
        let screen_w = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(0) } as f32;
        let screen_h = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(1) } as f32;

        let (w, h) = if self.show_settings {
            (470.0, 560.0)
        } else {
            (176.0, 44.0)
        };

        let (x, y) = if self.show_settings {
            (
                ((screen_w - w) / 2.0).max(10.0),
                ((screen_h - h) / 2.0).max(10.0),
            )
        } else {
            match self.settings.position.as_str() {
                "Arriba" => {
                    let target_x = if self.settings.x > 0 && (self.settings.x as f32) < screen_w - 50.0 {
                        self.settings.x as f32
                    } else {
                        640.0
                    };
                    (target_x, 37.0)
                }
                "Izquierda" => (
                    0.0,
                    ((screen_h - h) / 2.0).max(10.0),
                ),
                "Derecha" => (
                    (screen_w - w).max(10.0),
                    ((screen_h - h) / 2.0).max(10.0),
                ),
                _ => (
                    // "Abajo"
                    ((screen_w - w) / 2.0).max(10.0),
                    (screen_h - h).max(0.0),
                ),
            }
        };

        let hwnd = unsafe { find_own_window() };
        if !hwnd.is_null() {
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, GWL_STYLE,
                    SWP_FRAMECHANGED, SWP_SHOWWINDOW, WS_POPUP, WS_VISIBLE,
                    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_WINDOWEDGE,
                };

                // Remove all standard window chrome, titlebars, close buttons and borders
                SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE) as isize);

                // Set extended styles: toolwindow + topmost, and explicitly strip appwindow & windowedge
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                let new_ex = (ex | (WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize)
                    & !((WS_EX_APPWINDOW | WS_EX_WINDOWEDGE) as isize);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);

                SetWindowPos(
                    hwnd,
                    -1isize as windows_sys::Win32::Foundation::HWND, // HWND_TOPMOST
                    x as i32,
                    y as i32,
                    w as i32,
                    h as i32,
                    SWP_SHOWWINDOW | SWP_FRAMECHANGED,
                );
            }
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(Pos2::new(x, y)));
        self.update_window_region(ctx);
    }
}

impl eframe::App for DictateApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.process_events();
        let ctx = ui.ctx().clone();
        let dt = ctx.input(|i| i.stable_dt);
        self.anim_time += dt as f64;

        // Smoothly animate carapace expansion/collapse
        let target_open = if matches!(self.phase, Phase::Recording { .. } | Phase::Processing { .. }) {
            1.0
        } else {
            0.0
        };
        let prev_open = self.open;
        if self.open < target_open {
            self.open = (self.open + dt * 8.0).min(target_open);
        } else if self.open > target_open {
            self.open = (self.open - dt * 6.0).max(target_open);
        }

        if self.startup_frames < 5 {
            self.reposition_window(&ctx);
            self.startup_frames += 1;
            ctx.request_repaint();
        } else if (self.open - prev_open).abs() > 0.0001 && !self.show_settings {
            self.update_window_region(&ctx);
        }

        // Cancel with Escape key at any time while recording or processing
        if matches!(self.phase, Phase::Recording { .. } | Phase::Processing { .. }) {
            let esc = (unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x1B) } as u16 & 0x8000) != 0;
            if esc {
                self.cancel_dictation();
            }
        }

        // Keep animated redraw active when recording, processing, or easing
        if matches!(self.phase, Phase::Recording { .. } | Phase::Processing { .. }) || self.open > 0.001 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        ui.style_mut().visuals.window_fill = Color32::TRANSPARENT;
        ui.style_mut().visuals.panel_fill = Color32::TRANSPARENT;

        if self.show_settings {
            render_settings_window(self, ui, &ctx);
        } else {
            render_original_carapace(self, ui, &ctx);
        }
    }
}

fn render_original_carapace(app: &mut DictateApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(176.0, 44.0));
    let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

    if response.dragged() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if response.clicked() {
        app.toggle_dictation(DictationMode::Transcription);
    }
    if response.secondary_clicked() {
        if matches!(app.phase, Phase::Recording { .. } | Phase::Processing { .. }) {
            app.cancel_dictation();
        } else {
            app.show_settings = true;
            app.reposition_window(ctx);
        }
    }

    let level_f = if matches!(app.phase, Phase::Processing { .. }) {
        -1.0
    } else {
        (app.level.load(Ordering::Relaxed) as f32 / 32768.0).clamp(0.0, 1.0)
    };

    let painter = ui.painter_at(rect);
    paint::draw_carapace(
        &painter,
        176.0,
        44.0,
        app.state_color(),
        app.open,
        level_f,
        app.anim_time as f32,
        &app.settings.position,
    );
}

fn render_settings_window(app: &mut DictateApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    let full_rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(470.0, 560.0));
    let painter = ui.painter_at(full_rect);

    painter.rect_filled(full_rect, 14.0, Color32::from_rgb(16, 20, 28));
    painter.rect_stroke(full_rect, 14.0, Stroke::new(1.5, Color32::from_rgb(45, 55, 75)), egui::StrokeKind::Outside);

    egui::Frame::new().inner_margin(14.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (dot_rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 4.5, app.state_color());

            ui.heading(egui::RichText::new("Ajustes - HyprDictate").size(16.0).strong().color(Color32::from_rgb(230, 235, 245)));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(egui::RichText::new(" Volver al dock ").size(12.0).color(Color32::from_rgb(180, 195, 215))).clicked() {
                    app.show_settings = false;
                    app.reposition_window(ctx);
                }
            });
        });

        ui.add_space(8.0);

        ui.horizontal(|ui| {
            let is_server = app.active_tab == SettingsTab::Server;
            let s_col = if is_server { Color32::from_rgb(55, 175, 135) } else { Color32::from_rgb(140, 150, 165) };
            if ui.button(egui::RichText::new("Servidor y API Key").color(s_col).strong()).clicked() {
                app.active_tab = SettingsTab::Server;
            }

            let is_gen = app.active_tab == SettingsTab::General;
            let g_col = if is_gen { Color32::from_rgb(55, 175, 135) } else { Color32::from_rgb(140, 150, 165) };
            if ui.button(egui::RichText::new("General e IA").color(g_col).strong()).clicked() {
                app.active_tab = SettingsTab::General;
            }
        });

        ui.separator();
        ui.add_space(6.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            match app.active_tab {
                SettingsTab::Server => render_server_tab(app, ui),
                SettingsTab::General => render_general_tab(app, ui),
            }
        });
    });
}

fn render_server_tab(app: &mut DictateApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Configuracion de CLIProxyAPI / Servidor OpenAI").strong().color(Color32::from_rgb(210, 220, 235)));
    ui.add_space(6.0);

    ui.label(egui::RichText::new("URL Base del Servidor:").color(Color32::from_rgb(170, 180, 195)));
    ui.text_edit_singleline(&mut app.settings.base_url);
    ui.label(egui::RichText::new("Ejemplo: http://127.0.0.1:8317 o https://cpamc.sistemasias.com.ar").size(11.0).color(Color32::from_rgb(120, 130, 145)));

    ui.add_space(10.0);

    ui.label(egui::RichText::new("Clave API (API Key):").color(Color32::from_rgb(170, 180, 195)));
    ui.horizontal(|ui| {
        if app.show_api_key {
            ui.add(egui::TextEdit::singleline(&mut app.settings.api_key).desired_width(310.0));
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut app.settings.api_key)
                    .password(true)
                    .desired_width(310.0),
            );
        }

        if ui
            .button(if app.show_api_key { "Ocultar" } else { "Mostrar" })
            .clicked()
        {
            app.show_api_key = !app.show_api_key;
        }
    });
    ui.label(egui::RichText::new("Introduce aqui tu API Key. Se guardara permanentemente.").size(11.0).color(Color32::from_rgb(120, 130, 145)));

    ui.add_space(14.0);

    if ui
        .button(egui::RichText::new("Guardar servidor y recargar catalogo").strong().color(Color32::from_rgb(240, 245, 255)))
        .clicked()
    {
        if let Err(e) = app.settings.save() {
            app.status_message = format!("Error al guardar: {e}");
        } else {
            app.status_message = "Servidor y API Key guardados".into();
            app.refresh_catalog();
        }
    }

    ui.add_space(8.0);
    if !app.catalog_status.is_empty() {
        let is_ok = app.catalog_status.starts_with("OK");
        let col = if is_ok {
            Color32::from_rgb(70, 210, 130)
        } else {
            Color32::from_rgb(235, 80, 80)
        };
        ui.label(egui::RichText::new(&app.catalog_status).color(col).size(12.0));
    }
}

fn render_general_tab(app: &mut DictateApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Opciones Generales").strong().color(Color32::from_rgb(210, 220, 235)));
    ui.add_space(6.0);

    // Dock Position Selector
    ui.label(egui::RichText::new("Posicion del Dock en pantalla:").color(Color32::from_rgb(170, 180, 195)));
    let old_pos = app.settings.position.clone();
    egui::ComboBox::from_label("Borde")
        .selected_text(&app.settings.position)
        .show_ui(ui, |ui| {
            for &pos in &settings::DOCK_POSITIONS {
                ui.selectable_value(&mut app.settings.position, pos.into(), pos);
            }
        });

    if app.settings.position == "Arriba" {
        ui.add_space(4.0);
        let screen_w = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(0) };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Posición horizontal (X):").size(12.0).color(Color32::from_rgb(150, 160, 175)));
            if ui.add(egui::Slider::new(&mut app.settings.x, 10..=(screen_w - 186)).text("px")).changed() {
                app.reposition_window(ui.ctx());
            }
        });
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("Izquierda de Fecha/Hora (Recomendado)").size(11.0)).clicked() {
                app.settings.x = 640;
                app.reposition_window(ui.ctx());
            }
            if ui.button(egui::RichText::new("Centro").size(11.0)).clicked() {
                app.settings.x = ((screen_w - 176) / 2).max(10);
                app.reposition_window(ui.ctx());
            }
        });
    }

    if old_pos != app.settings.position {
        app.reposition_window(ui.ctx());
    }

    ui.add_space(8.0);

    // Model Selector
    ui.label(egui::RichText::new("Modelo de Transcripcion:").color(Color32::from_rgb(170, 180, 195)));
    ui.horizontal(|ui| {
        ui.label("Filtrar:");
        ui.text_edit_singleline(&mut app.model_filter);
    });

    let filtered_models: Vec<&String> = app
        .models
        .iter()
        .filter(|m| {
            app.model_filter.is_empty()
                || m.to_lowercase().contains(&app.model_filter.to_lowercase())
        })
        .collect();

    egui::ComboBox::from_label("Modelo")
        .selected_text(if app.settings.model.is_empty() {
            "Ninguno seleccionado"
        } else {
            &app.settings.model
        })
        .show_ui(ui, |ui| {
            for m in filtered_models {
                ui.selectable_value(&mut app.settings.model, m.clone(), m);
            }
        });

    ui.add_space(8.0);

    // Microphone Selector
    ui.label(egui::RichText::new("Microfono de entrada:").color(Color32::from_rgb(170, 180, 195)));
    egui::ComboBox::from_label("Dispositivo")
        .selected_text(if app.settings.microphone.is_empty() {
            "Predeterminado del sistema"
        } else {
            &app.settings.microphone
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut app.settings.microphone,
                String::new(),
                "Predeterminado del sistema",
            );
            for mic in &app.microphones {
                ui.selectable_value(
                    &mut app.settings.microphone,
                    mic.name.clone(),
                    &mic.description,
                );
            }
        });

    ui.add_space(8.0);

    // Shortcut Selectors & Prompts for Both Modes
    ui.separator();
    ui.add_space(4.0);

    // MODE 1: STANDARD TRANSCRIPTION
    ui.label(
        egui::RichText::new("Modo 1: Transcripción (Dictado fiel)")
            .strong()
            .color(Color32::from_rgb(235, 100, 110)),
    );
    ui.label(
        egui::RichText::new("Transcribe exactamente lo dicho, corrigiendo titubeos, muletillas y signos.")
            .size(11.0)
            .color(Color32::from_rgb(130, 140, 155)),
    );

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Atajo Dictado:").color(Color32::from_rgb(170, 180, 195)));
        let old_sc = app.settings.shortcut.clone();
        egui::ComboBox::from_id_salt("combo_shortcut_dictate")
            .selected_text(if app.settings.shortcut.is_empty() {
                "Desactivado"
            } else {
                &app.settings.shortcut
            })
            .show_ui(ui, |ui| {
                for &sc in &shortcut::PRESET_SHORTCUTS {
                    let label = if sc.is_empty() { "Desactivado" } else { sc };
                    ui.selectable_value(&mut app.settings.shortcut, sc.into(), label);
                }
            });

        if old_sc != app.settings.shortcut {
            app.sync_shortcut();
        }
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Prompt de Transcripción:").color(Color32::from_rgb(170, 180, 195)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(egui::RichText::new("Restablecer por defecto").size(11.0)).clicked() {
                app.settings.prompt = settings::DEFAULT_PROMPT.into();
            }
        });
    });
    ui.add(
        egui::TextEdit::multiline(&mut app.settings.prompt)
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(4.0);

    // MODE 2: FORMAL REDACTION
    ui.label(
        egui::RichText::new("Modo 2: Redacción Formal (Asistente Ejecutivo)")
            .strong()
            .color(Color32::from_rgb(100, 160, 255)),
    );
    ui.label(
        egui::RichText::new("Escucha tus notas o ideas y redacta un texto formal, profesional y elegante.")
            .size(11.0)
            .color(Color32::from_rgb(130, 140, 155)),
    );

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Atajo Formal:").color(Color32::from_rgb(170, 180, 195)));
        let old_scf = app.settings.shortcut_formal.clone();
        egui::ComboBox::from_id_salt("combo_shortcut_formal")
            .selected_text(if app.settings.shortcut_formal.is_empty() {
                "Desactivado"
            } else {
                &app.settings.shortcut_formal
            })
            .show_ui(ui, |ui| {
                for &sc in &shortcut::PRESET_SHORTCUTS {
                    let label = if sc.is_empty() { "Desactivado" } else { sc };
                    ui.selectable_value(&mut app.settings.shortcut_formal, sc.into(), label);
                }
            });

        if old_scf != app.settings.shortcut_formal {
            app.sync_shortcut();
        }
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Prompt Formal:").color(Color32::from_rgb(170, 180, 195)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(egui::RichText::new("Restablecer por defecto").size(11.0)).clicked() {
                app.settings.prompt_formal = settings::DEFAULT_PROMPT_FORMAL.into();
            }
        });
    });
    ui.add(
        egui::TextEdit::multiline(&mut app.settings.prompt_formal)
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(4.0);

    // Autostart with Windows
    if ui
        .checkbox(
            &mut app.settings.autostart,
            "Iniciar automáticamente con Windows",
        )
        .changed()
    {
        let _ = crate::autostart::set_autostart(app.settings.autostart);
    }

    ui.add_space(6.0);

    // Temperature
    ui.label(egui::RichText::new("Temperatura de IA (Precisión / Creatividad):").color(Color32::from_rgb(170, 180, 195)));
    ui.add(egui::Slider::new(&mut app.settings.temperature, 0.0..=1.0).text("Precisión"));

    ui.add_space(6.0);
    ui.small("Tip: Puedes mantener presionado el atajo para hablar y soltarlo para transcribir, o presionarlo una vez para iniciar y otra para finalizar. Pulsa Escape para cancelar.");

    ui.add_space(14.0);
    ui.separator();
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if ui
            .button(egui::RichText::new("Guardar cambios").strong().color(Color32::from_rgb(240, 245, 255)))
            .clicked()
        {
            app.sync_shortcut();
            let _ = crate::autostart::set_autostart(app.settings.autostart);
            if let Err(e) = app.settings.save() {
                app.status_message = format!("Error al guardar: {e}");
            } else {
                app.status_message = "Cambios guardados con éxito".into();
            }
        }

        if ui
            .button(egui::RichText::new("Volver al dock").color(Color32::from_rgb(180, 195, 215)))
            .clicked()
        {
            app.show_settings = false;
            app.reposition_window(ui.ctx());
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(egui::RichText::new("Cerrar y Salir de HyprDictate").strong().color(Color32::from_rgb(235, 80, 80)))
                .clicked()
            {
                std::process::exit(0);
            }
        });
    });
}
