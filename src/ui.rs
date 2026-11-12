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

const SETTINGS_W: f32 = 860.0;
const SETTINGS_H: f32 = 700.0;

#[derive(PartialEq, Clone, Copy)]
pub enum SettingsTab {
    General,
    Ai,
    Server,
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
    pub model_filter_formal: String,

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
        cc: &eframe::CreationContext<'_>,
        initial_settings: Settings,
        shared_shortcuts: Arc<RwLock<shortcut::ShortcutSettings>>,
    ) -> Self {
        configure_theme(&cc.egui_ctx);
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
            active_tab: SettingsTab::General,
            show_api_key: false,
            status_message: "Listo".into(),
            catalog_status: String::new(),
            model_filter: String::new(),
            model_filter_formal: String::new(),
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

    pub fn effective_model(&self, mode: DictationMode) -> &str {
        match mode {
            DictationMode::Transcription => self.settings.model.trim(),
            DictationMode::Formal => {
                let mf = self.settings.model_formal.trim();
                if mf.is_empty() {
                    self.settings.model.trim()
                } else {
                    mf
                }
            }
        }
    }

    pub fn start_dictation(&mut self, mode: DictationMode) {
        if !matches!(self.phase, Phase::Idle | Phase::Error) {
            return;
        }

        if self.effective_model(mode).is_empty() {
            if let Some(first) = self.models.first() {
                match mode {
                    DictationMode::Transcription => self.settings.model = first.clone(),
                    DictationMode::Formal => self.settings.model_formal = first.clone(),
                }
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
        let model = self.effective_model(target_mode).to_owned();
        let temp = match target_mode {
            DictationMode::Transcription => self.settings.temperature,
            DictationMode::Formal => self.settings.temperature_formal,
        };
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
                        self.catalog_status = format!("Conectado: {} modelos disponibles", self.models.len());
                        if self.settings.model.trim().is_empty() && !self.models.is_empty() {
                            self.settings.model = self.models[0].clone();
                        }
                        if self.settings.model_formal.trim().is_empty() && !self.models.is_empty() {
                            self.settings.model_formal = if self.settings.model.trim().is_empty() {
                                self.models[0].clone()
                            } else {
                                self.settings.model.clone()
                            };
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
                let w = (SETTINGS_W * ppp).round() as i32;
                let h = (SETTINGS_H * ppp).round() as i32;
                let r = (32.0 * ppp).round() as i32;
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
            (SETTINGS_W, SETTINGS_H)
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
                    (target_x, -1.0)
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

const BG_WINDOW: Color32 = Color32::from_rgb(13, 17, 24);
const BG_CARD: Color32 = Color32::from_rgb(20, 26, 36);
const BG_SURFACE: Color32 = Color32::from_rgb(27, 34, 48);
const BG_SURFACE_HOVER: Color32 = Color32::from_rgb(36, 45, 62);
const BORDER_SUBTLE: Color32 = Color32::from_rgb(38, 48, 66);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(232, 238, 248);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(152, 164, 184);
const TEXT_MUTED: Color32 = Color32::from_rgb(106, 118, 138);
const ACCENT_EMERALD: Color32 = Color32::from_rgb(55, 175, 135);
const ACCENT_EMERALD_DARK: Color32 = Color32::from_rgb(32, 118, 90);
const DANGER_RED: Color32 = Color32::from_rgb(225, 82, 88);

fn configure_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.interact_size = Vec2::new(40.0, 30.0);
    style.spacing.slider_width = 220.0;
    style.spacing.combo_width = 240.0;

    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.window_fill = BG_WINDOW;
    v.panel_fill = BG_WINDOW;
    v.faint_bg_color = BG_CARD;
    v.extreme_bg_color = Color32::from_rgb(10, 13, 19);
    v.selection.bg_fill = ACCENT_EMERALD_DARK;
    v.selection.stroke = Stroke::new(1.0, ACCENT_EMERALD);

    let cr = egui::CornerRadius::same(8);
    v.window_corner_radius = egui::CornerRadius::same(16);
    v.menu_corner_radius = egui::CornerRadius::same(10);

    v.widgets.noninteractive.bg_fill = BG_CARD;
    v.widgets.noninteractive.weak_bg_fill = BG_CARD;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    v.widgets.noninteractive.corner_radius = cr;

    v.widgets.inactive.bg_fill = BG_SURFACE;
    v.widgets.inactive.weak_bg_fill = BG_SURFACE;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER_SUBTLE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.inactive.corner_radius = cr;

    v.widgets.hovered.bg_fill = BG_SURFACE_HOVER;
    v.widgets.hovered.weak_bg_fill = BG_SURFACE_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_EMERALD);
    v.widgets.hovered.fg_stroke = Stroke::new(1.2, TEXT_PRIMARY);
    v.widgets.hovered.corner_radius = cr;

    v.widgets.active.bg_fill = Color32::from_rgb(42, 54, 74);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(42, 54, 74);
    v.widgets.active.bg_stroke = Stroke::new(1.2, ACCENT_EMERALD);
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.active.corner_radius = cr;

    style.visuals = v;
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn section_card(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    egui::Frame::new()
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            let inner_w = (w - 24.0).max(100.0);
            ui.vertical(|ui| {
                ui.set_min_width(inner_w);
                ui.set_max_width(inner_w);
                ui.label(egui::RichText::new(title).size(13.5).strong().color(TEXT_PRIMARY));
                ui.add_space(6.0);
                add_contents(ui);
            });
        });
}

fn tab_button(ui: &mut egui::Ui, label: &str, active: bool, width: f32) -> egui::Response {
    let fill = if active {
        Color32::from_rgb(28, 48, 46)
    } else {
        Color32::TRANSPARENT
    };
    let stroke = if active {
        Stroke::new(1.0, ACCENT_EMERALD)
    } else {
        Stroke::NONE
    };
    let text_color = if active { ACCENT_EMERALD } else { TEXT_SECONDARY };
    ui.add(
        egui::Button::new(egui::RichText::new(label).size(13.0).strong().color(text_color))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(egui::CornerRadius::same(8))
            .min_size(Vec2::new(width, 32.0)),
    )
}

fn render_settings_window(app: &mut DictateApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    let full_rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(SETTINGS_W, SETTINGS_H));
    let painter = ui.painter_at(full_rect);

    painter.rect_filled(full_rect, 16.0, BG_WINDOW);
    painter.rect_stroke(
        full_rect.shrink(0.5),
        16.0,
        Stroke::new(1.0, BORDER_SUBTLE),
        egui::StrokeKind::Inside,
    );

    egui::Frame::new().inner_margin(egui::Margin::same(20)).show(ui, |ui| {
        ui.set_width(SETTINGS_W - 40.0);
        ui.set_height(SETTINGS_H - 40.0);

        // Header with dedicated drag handle that NEVER overlaps the button
        ui.horizontal(|ui| {
            let (dot_rect, _) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 4.5, app.state_color());

            ui.label(
                egui::RichText::new("HyprDictate")
                    .size(17.0)
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            ui.label(
                egui::RichText::new("· Configuración")
                    .size(13.5)
                    .color(TEXT_MUTED),
            );

            // Empty spacer between title and button acts as the drag handle
            let btn_reserve = 132.0;
            let drag_w = (ui.available_width() - btn_reserve).max(20.0);
            let drag_resp = ui.allocate_response(Vec2::new(drag_w, 30.0), egui::Sense::drag());
            if drag_resp.dragged() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Volver al dock")
                                .size(12.5)
                                .color(TEXT_PRIMARY),
                        )
                        .fill(BG_SURFACE)
                        .stroke(Stroke::new(1.0, BORDER_SUBTLE))
                        .corner_radius(egui::CornerRadius::same(8))
                        .min_size(Vec2::new(118.0, 30.0)),
                    )
                    .clicked()
                {
                    app.show_settings = false;
                    app.reposition_window(ctx);
                }
            });
        });

        ui.add_space(12.0);

        // Segmented Tab Bar
        egui::Frame::new()
            .fill(BG_CARD)
            .stroke(Stroke::new(1.0, BORDER_SUBTLE))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                let inner_w = ui.available_width();
                let tab_w = ((inner_w - 20.0) / 3.0).max(100.0);
                ui.horizontal(|ui| {
                    if tab_button(ui, "General", app.active_tab == SettingsTab::General, tab_w).clicked() {
                        app.active_tab = SettingsTab::General;
                    }
                    if tab_button(ui, "Modelo e IA", app.active_tab == SettingsTab::Ai, tab_w).clicked() {
                        app.active_tab = SettingsTab::Ai;
                    }
                    if tab_button(ui, "Servidor API", app.active_tab == SettingsTab::Server, tab_w).clicked() {
                        app.active_tab = SettingsTab::Server;
                    }
                });
            });

        ui.add_space(12.0);

        // Scrollable Content Area (reserve exact space for sticky footer)
        let footer_h = 68.0;
        let scroll_h = (ui.available_height() - footer_h).max(120.0);
        ui.allocate_ui(Vec2::new(ui.available_width(), scroll_h), |ui| {
            egui::ScrollArea::vertical()
                .max_height(scroll_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match app.active_tab {
                        SettingsTab::General => render_general_tab(app, ui),
                        SettingsTab::Ai => render_ai_tab(app, ui),
                        SettingsTab::Server => render_server_tab(app, ui),
                    }
                    ui.add_space(6.0);
                });
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // Sticky Footer
        ui.horizontal(|ui| {
            let status_col = if app.status_message.starts_with("Error") {
                DANGER_RED
            } else if app.status_message.contains("guardad") || app.status_message == "Listo" {
                ACCENT_EMERALD
            } else {
                TEXT_SECONDARY
            };
            let (sdot, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(sdot.center(), 3.5, status_col);
            ui.label(
                egui::RichText::new(&app.status_message)
                    .size(11.5)
                    .color(status_col),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Guardar")
                                .size(12.5)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(ACCENT_EMERALD_DARK)
                        .stroke(Stroke::new(1.0, ACCENT_EMERALD))
                        .corner_radius(egui::CornerRadius::same(8)),
                    )
                    .clicked()
                {
                    app.sync_shortcut();
                    let _ = crate::autostart::set_autostart(app.settings.autostart);
                    if let Err(e) = app.settings.save() {
                        app.status_message = format!("Error al guardar: {e}");
                    } else {
                        app.status_message = "Cambios guardados".into();
                    }
                }

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Salir app")
                                .size(12.0)
                                .color(DANGER_RED),
                        )
                        .fill(Color32::from_rgb(38, 22, 26))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(90, 42, 46)))
                        .corner_radius(egui::CornerRadius::same(8)),
                    )
                    .clicked()
                {
                    std::process::exit(0);
                }
            });
        });
    });
}

fn render_server_tab(app: &mut DictateApp, ui: &mut egui::Ui) {
    section_card(ui, "API compatible con OpenAI", |ui| {
        ui.label(
            egui::RichText::new(
                "Conecta con CLIProxyAPI, OpenAI, OpenRouter, Groq o cualquier servidor /v1/chat/completions.",
            )
            .size(11.5)
            .color(TEXT_MUTED),
        );
        ui.add_space(6.0);

        ui.label(egui::RichText::new("URL Base del Servidor").size(12.0).color(TEXT_SECONDARY));
        ui.add(
            egui::TextEdit::singleline(&mut app.settings.base_url)
                .desired_width(f32::INFINITY)
                .hint_text("http://127.0.0.1:8317"),
        );
        ui.label(
            egui::RichText::new("Ej: http://127.0.0.1:8317  ·  https://api.openai.com/v1")
                .size(11.0)
                .color(TEXT_MUTED),
        );

        ui.add_space(8.0);

        ui.label(egui::RichText::new("Clave API (Bearer Token)").size(12.0).color(TEXT_SECONDARY));
        ui.horizontal(|ui| {
            let edit_w = (ui.available_width() - 78.0).max(160.0);
            if app.show_api_key {
                ui.add(
                    egui::TextEdit::singleline(&mut app.settings.api_key)
                        .desired_width(edit_w)
                        .hint_text("sk-..."),
                );
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut app.settings.api_key)
                        .password(true)
                        .desired_width(edit_w)
                        .hint_text("sk-..."),
                );
            }

            if ui
                .button(if app.show_api_key { "Ocultar" } else { "Mostrar" })
                .clicked()
            {
                app.show_api_key = !app.show_api_key;
            }
        });
    });

    ui.add_space(10.0);

    section_card(ui, "Conexión y catálogo", |ui| {
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Guardar y recargar modelos")
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(ACCENT_EMERALD_DARK)
                    .stroke(Stroke::new(1.0, ACCENT_EMERALD)),
                )
                .clicked()
            {
                app.models.clear();
                if let Err(e) = app.settings.save() {
                    app.status_message = format!("Error al guardar: {e}");
                } else {
                    app.status_message = "Servidor guardado".into();
                    app.refresh_catalog();
                }
            }
        });

        if !app.catalog_status.is_empty() {
            ui.add_space(6.0);
            let is_ok = app.catalog_status.starts_with("Conectado");
            let col = if is_ok {
                ACCENT_EMERALD
            } else if app.catalog_status.starts_with("Cargando") {
                TEXT_SECONDARY
            } else {
                DANGER_RED
            };
            ui.label(egui::RichText::new(&app.catalog_status).color(col).size(12.0));
        }
    });
}

fn render_general_tab(app: &mut DictateApp, ui: &mut egui::Ui) {
    section_card(ui, "Posición del dock", |ui| {
        let old_pos = app.settings.position.clone();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Borde:").size(12.5).color(TEXT_SECONDARY));
            egui::ComboBox::from_id_salt("combo_dock_pos")
                .selected_text(&app.settings.position)
                .show_ui(ui, |ui| {
                    for &pos in &settings::DOCK_POSITIONS {
                        ui.selectable_value(&mut app.settings.position, pos.into(), pos);
                    }
                });
        });

        if old_pos != app.settings.position {
            app.reposition_window(ui.ctx());
        }

        if app.settings.position == "Arriba" {
            let screen_w = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(0) };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Posición X:").size(12.0).color(TEXT_SECONDARY));
                if ui
                    .add(egui::Slider::new(&mut app.settings.x, 10..=(screen_w - 186)).text("px"))
                    .changed()
                {
                    app.reposition_window(ui.ctx());
                }
                if ui
                    .button(egui::RichText::new("Izquierda de Fecha/Hora").size(11.5))
                    .clicked()
                {
                    app.settings.x = 640;
                    app.reposition_window(ui.ctx());
                }
                if ui
                    .button(egui::RichText::new("Centrar").size(11.5))
                    .clicked()
                {
                    app.settings.x = ((screen_w - 176) / 2).max(10);
                    app.reposition_window(ui.ctx());
                }
            });
        }
    });

    ui.add_space(8.0);

    section_card(ui, "Audio e inicio", |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Micrófono:").size(12.5).color(TEXT_SECONDARY));
            let mic_label = if app.settings.microphone.is_empty() {
                "Predeterminado del sistema"
            } else {
                &app.settings.microphone
            };
            egui::ComboBox::from_id_salt("combo_microphone")
                .width(290.0)
                .selected_text(mic_label)
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
        });

        ui.add_space(4.0);
        if ui
            .checkbox(
                &mut app.settings.autostart,
                "Iniciar automáticamente con Windows",
            )
            .changed()
        {
            let _ = crate::autostart::set_autostart(app.settings.autostart);
        }
    });

    ui.add_space(8.0);

    section_card(ui, "Atajos globales", |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Dictado fiel:")
                    .size(12.5)
                    .color(Color32::from_rgb(235, 115, 125)),
            );
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

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Redacción formal:")
                    .size(12.5)
                    .color(Color32::from_rgb(115, 175, 255)),
            );
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

        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Mantén pulsado para hablar y suelta para transcribir, o pulsa una vez para iniciar y otra para terminar. Escape cancela.",
            )
            .size(11.0)
            .color(TEXT_MUTED),
        );
    });
}

fn render_model_picker(
    ui: &mut egui::Ui,
    id_salt: &str,
    models: &[String],
    selected_model: &mut String,
    filter: &mut String,
    hint: &str,
) {
    if models.is_empty() {
        ui.add(
            egui::TextEdit::singleline(selected_model)
                .desired_width(f32::INFINITY)
                .hint_text(hint),
        );
        return;
    }

    let avail = ui.available_width();
    ui.horizontal(|ui| {
        let clear_w = if filter.is_empty() { 0.0 } else { 32.0 };
        ui.add(
            egui::TextEdit::singleline(filter)
                .desired_width((avail - clear_w - 6.0).max(120.0))
                .hint_text("Filtrar modelos..."),
        );
        if !filter.is_empty() && ui.button(egui::RichText::new("×").size(12.0)).clicked() {
            filter.clear();
        }
    });

    let q = filter.trim().to_lowercase();
    let filtered: Vec<&String> = models
        .iter()
        .filter(|m| q.is_empty() || m.to_lowercase().contains(&q))
        .collect();

    let display = if selected_model.trim().is_empty() {
        "Ninguno seleccionado"
    } else {
        selected_model.as_str()
    };
    egui::ComboBox::from_id_salt(id_salt)
        .width(avail.max(180.0))
        .selected_text(display)
        .show_ui(ui, |ui| {
            if filtered.is_empty() {
                ui.label(egui::RichText::new("Sin coincidencias").size(11.5).color(TEXT_MUTED));
            } else {
                for m in filtered {
                    ui.selectable_value(selected_model, m.clone(), m.as_str());
                }
            }
        });
}

fn render_mode_card(
    ui: &mut egui::Ui,
    title: &str,
    id_salt: &str,
    models: &[String],
    selected_model: &mut String,
    filter: &mut String,
    hint: &str,
    temperature: &mut f64,
    prompt: &mut String,
    default_prompt: &str,
) {
    section_card(ui, title, |ui| {
        ui.style_mut().spacing.slider_width = 180.0;

        render_model_picker(ui, id_salt, models, selected_model, filter, hint);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Temperatura:").size(12.0).color(TEXT_SECONDARY));
            ui.add(egui::Slider::new(temperature, 0.0..=1.0).step_by(0.05));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Prompt:").size(12.0).color(TEXT_SECONDARY));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(egui::RichText::new("Restablecer").size(11.0)).clicked() {
                    *prompt = default_prompt.to_owned();
                }
            });
        });

        let text_w = ui.available_width();
        egui::ScrollArea::vertical()
            .id_salt(format!("{id_salt}_prompt_scroll"))
            .max_height(235.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(prompt)
                        .desired_rows(11)
                        .desired_width(text_w),
                );
            });
    });
}

fn render_ai_tab(app: &mut DictateApp, ui: &mut egui::Ui) {
    section_card(ui, "Catálogo de modelos", |ui| {
        ui.horizontal(|ui| {
            if !app.models.is_empty() {
                ui.label(
                    egui::RichText::new(format!("{} modelos cargados", app.models.len()))
                        .size(12.0)
                        .color(ACCENT_EMERALD),
                );
            } else {
                ui.label(
                    egui::RichText::new("Sin catálogo: escribe el ID del modelo manualmente o recarga.")
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Recargar modelos").clicked() {
                    app.refresh_catalog();
                }
            });
        });
    });

    ui.add_space(10.0);

    let gap = 12.0;
    let total_w = ui.available_width();
    let col_w = ((total_w - gap) / 2.0).floor().max(260.0);

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(col_w, 430.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                render_mode_card(
                    ui,
                    "Modo 1 · Transcripción",
                    "combo_model_transcription",
                    &app.models,
                    &mut app.settings.model,
                    &mut app.model_filter,
                    "Ej: gemini-2.5-flash",
                    &mut app.settings.temperature,
                    &mut app.settings.prompt,
                    settings::DEFAULT_PROMPT,
                );
            },
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            Vec2::new(col_w, 430.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                render_mode_card(
                    ui,
                    "Modo 2 · Redacción Formal",
                    "combo_model_formal",
                    &app.models,
                    &mut app.settings.model_formal,
                    &mut app.model_filter_formal,
                    "Ej: gemini-2.5-pro",
                    &mut app.settings.temperature_formal,
                    &mut app.settings.prompt_formal,
                    settings::DEFAULT_PROMPT_FORMAL,
                );
            },
        );
    });
}
