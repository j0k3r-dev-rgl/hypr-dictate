use crate::{App, Event, paint::Modal};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, SeekFrom, Write},
    os::{
        fd::{AsFd, FromRawFd},
        unix::fs::FileExt,
    },
    sync::mpsc,
    time::Duration,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
};
use xkbcommon::xkb;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, Layer},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity},
};

#[derive(Clone, Debug)]
pub struct OutputItem {
    pub name: String,
    pub output: wl_output::WlOutput,
}

const SHORTCUTS: [&str; 6] = [
    "",
    "CTRL + SHIFT + D",
    "CTRL + ALT + D",
    "SUPER + SHIFT + D",
    "SUPER + F9",
    "SUPER + F10",
];

fn memory_file() -> Result<File> {
    let fd = unsafe { libc::memfd_create(c"hypr-dictate".as_ptr(), libc::MFD_CLOEXEC) };
    ensure!(
        fd >= 0,
        "No se pudo crear memfd: {}",
        std::io::Error::last_os_error()
    );
    // memfd_create returned a fresh descriptor; File takes sole ownership.
    Ok(unsafe { File::from_raw_fd(fd) })
}

struct SurfaceBuffers {
    file: File,
    pool: wl_shm_pool::WlShmPool,
    buffers: [wl_buffer::WlBuffer; 2],
    busy: [bool; 2],
    width: i32,
    height: i32,
    frame_size: usize,
}
impl SurfaceBuffers {
    fn new(shm: &wl_shm::WlShm, width: i32, height: i32, qh: &QueueHandle<Ui>) -> Result<Self> {
        let frame_size = (width as usize)
            .checked_mul(height as usize)
            .and_then(|size| size.checked_mul(4))
            .filter(|size| width > 0 && height > 0 && *size <= i32::MAX as usize / 2)
            .context("Tamaño de superficie inválido")?;
        let file = memory_file()?;
        file.set_len((frame_size * 2) as u64)?;
        let pool = shm.create_pool(file.as_fd(), (frame_size * 2) as i32, qh, ());
        let buffers = std::array::from_fn(|slot| {
            pool.create_buffer(
                (slot * frame_size) as i32,
                width,
                height,
                width * 4,
                wl_shm::Format::Argb8888,
                qh,
                (),
            )
        });
        Ok(Self {
            file,
            pool,
            buffers,
            busy: [false; 2],
            width,
            height,
            frame_size,
        })
    }
    fn available(&self) -> bool {
        self.busy.contains(&false)
    }
    fn attach(&mut self, surface: &wl_surface::WlSurface, pixels: &[u8]) -> Result<bool> {
        ensure!(pixels.len() == self.frame_size, "Tamaño de frame inválido");
        let Some(slot) = self.busy.iter().position(|busy| !busy) else {
            return Ok(false);
        };
        self.file
            .write_all_at(pixels, (slot * self.frame_size) as u64)?;
        self.busy[slot] = true;
        surface.attach(Some(&self.buffers[slot]), 0, 0);
        surface.damage_buffer(0, 0, self.width, self.height);
        surface.commit();
        Ok(true)
    }
    fn release(&mut self, buffer: &wl_buffer::WlBuffer) {
        if let Some(slot) = self.buffers.iter().position(|item| item == buffer) {
            self.busy[slot] = false;
        }
    }
}
impl Drop for SurfaceBuffers {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.destroy();
        }
        self.pool.destroy();
    }
}

pub struct Ui {
    connection: Connection,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    surface: Option<wl_surface::WlSurface>,
    layer: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    modal_surface: Option<wl_surface::WlSurface>,
    modal_layer: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    pointer: Option<wl_pointer::WlPointer>,
    tx: mpsc::Sender<Event>,
    configured: bool,
    modal_configured: bool,
    closed: bool,
    color: [u8; 3],
    pub models: Vec<String>,
    pub message: String,
    draft: crate::settings::Settings,
    saved: crate::settings::Settings,
    panel: bool,
    open_select: Option<usize>,
    in_modal: bool,
    pointer_xy: (f64, f64),
    press: Option<(f64, f64, i32)>,
    dragged: bool,
    tick: f64,
    open_progress: f64,
    pub level: f64,
    account_index: usize,
    model_index: usize,
    shortcut_index: usize,
    pub microphones: Vec<crate::settings::Microphone>,
    microphone_index: usize,
    pub monitors: Vec<crate::settings::MonitorInfo>,
    outputs: Vec<OutputItem>,
    monitor_index: usize,
    dock_buffers: Option<SurfaceBuffers>,
    modal_buffers: Option<SurfaceBuffers>,
    dock_dirty: bool,
    pub dirty: bool,
    max_x: i32,
    pub tab: usize,
    pub prompt_cursor: usize,
    pub prompt_focused: bool,
    server_cursor: usize,
    server_focused: bool,
    seat_keyboard: Option<wl_keyboard::WlKeyboard>,
    xkb_context: xkb::Context,
    xkb_state: Option<xkb::State>,
}
impl Ui {
    fn new(
        connection: Connection,
        app: &App,
        microphones: Vec<crate::settings::Microphone>,
        monitors: Vec<crate::settings::MonitorInfo>,
        max_x: i32,
    ) -> Self {
        Self {
            connection,
            compositor: None,
            shm: None,
            seat: None,
            shell: None,
            manager: None,
            keyboard: None,
            surface: None,
            layer: None,
            modal_surface: None,
            modal_layer: None,
            pointer: None,
            tx: app.tx.clone(),
            configured: false,
            modal_configured: false,
            closed: false,
            color: app.color(),
            models: Vec::new(),
            message: "Cargando catálogo…".into(),
            draft: app.settings.clone(),
            saved: app.settings.clone(),
            panel: false,
            open_select: None,
            in_modal: false,
            pointer_xy: (0., 0.),
            press: None,
            dragged: false,
            tick: 0.,
            open_progress: 0.,
            level: 0.,
            account_index: 0,
            model_index: 0,
            shortcut_index: SHORTCUTS
                .iter()
                .position(|s| *s == app.settings.shortcut)
                .unwrap_or(0),
            microphone_index: microphones
                .iter()
                .position(|m| m.name == app.settings.microphone)
                .map(|i| i + 1)
                .unwrap_or(0),
            microphones,
            outputs: Vec::new(),
            monitor_index: monitors
                .iter()
                .position(|m| m.name == app.settings.monitor)
                .map(|i| i + 1)
                .unwrap_or(0),
            monitors,
            dock_buffers: None,
            modal_buffers: None,
            dock_dirty: true,
            dirty: true,
            max_x,
            tab: 0,
            prompt_cursor: app.settings.prompt.chars().count(),
            prompt_focused: false,
            server_cursor: app.settings.base_url.chars().count(),
            server_focused: false,
            seat_keyboard: None,
            xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
        }
    }
    fn discard_draft(&mut self) {
        self.draft = self.saved.clone();
        self.sync_model_indices();
        self.shortcut_index = SHORTCUTS
            .iter()
            .position(|s| *s == self.draft.shortcut)
            .unwrap_or(0);
        self.microphone_index = self
            .microphones
            .iter()
            .position(|m| m.name == self.draft.microphone)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.monitor_index = self
            .monitors
            .iter()
            .position(|m| m.name == self.draft.monitor)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.prompt_cursor = self.draft.prompt.chars().count();
        self.server_cursor = self.draft.base_url.chars().count();
        self.open_select = None;
        self.prompt_focused = false;
        self.server_focused = false;
        self.dirty = true;
    }
    pub fn settings_saved(&mut self, settings: &crate::settings::Settings) {
        self.saved = settings.clone();
    }
    pub fn server_saved(&mut self, base_url: &str) {
        self.saved.base_url = base_url.into();
        self.draft.base_url = base_url.into();
        self.models.clear();
        self.sync_model_indices();
        self.message = "Servidor guardado · cargando catálogo…".into();
        self.dirty = true;
    }
    fn edit_server(&mut self, sym: xkb::Keysym, text: &str) {
        let mut chars: Vec<char> = self.draft.base_url.chars().collect();
        self.server_cursor = self.server_cursor.min(chars.len());
        match sym {
            xkb::Keysym::BackSpace if self.server_cursor > 0 => {
                self.server_cursor -= 1;
                chars.remove(self.server_cursor);
            }
            xkb::Keysym::Delete if self.server_cursor < chars.len() => {
                chars.remove(self.server_cursor);
            }
            xkb::Keysym::Left => self.server_cursor = self.server_cursor.saturating_sub(1),
            xkb::Keysym::Right => self.server_cursor = (self.server_cursor + 1).min(chars.len()),
            xkb::Keysym::Home => self.server_cursor = 0,
            xkb::Keysym::End => self.server_cursor = chars.len(),
            _ if !text.is_empty() && !text.chars().any(char::is_control) => {
                let inserted: Vec<char> = text.chars().collect();
                let count = inserted.len();
                chars.splice(self.server_cursor..self.server_cursor, inserted);
                self.server_cursor += count;
            }
            _ => return,
        }
        self.draft.base_url = chars.into_iter().collect();
        self.dirty = true;
    }
    pub fn position_saved(&mut self, x: i32) {
        self.saved.x = x;
        self.draft.x = x;
    }
    fn save_position(&self) {
        let _ = self.tx.send(Event::SavePosition(self.draft.x));
    }
    fn animate_dock(&mut self, color: [u8; 3]) {
        if self.color != color {
            self.dirty = true;
            self.color = color;
        }
        self.tick += 1.;
        self.dock_dirty = true;
    }
    fn draw(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if self.configured && self.dock_buffers.is_none() {
            self.dock_buffers = Some(SurfaceBuffers::new(
                self.shm.as_ref().unwrap(),
                176,
                44,
                qh,
            )?);
        }
        if self.panel && self.modal_configured && self.modal_buffers.is_none() {
            self.modal_buffers = Some(SurfaceBuffers::new(
                self.shm.as_ref().unwrap(),
                540,
                crate::paint::MODAL_HEIGHT,
                qh,
            )?);
        }
        if self.configured
            && self.dock_dirty
            && self
                .dock_buffers
                .as_ref()
                .is_some_and(SurfaceBuffers::available)
        {
            let pixels = crate::paint::shell(
                176,
                44,
                self.color,
                self.open_progress,
                self.level,
                self.tick,
            )?;
            if self
                .dock_buffers
                .as_mut()
                .unwrap()
                .attach(self.surface.as_ref().unwrap(), &pixels)?
            {
                self.dock_dirty = false;
            }
        }
        if self.panel
            && self.modal_configured
            && self.dirty
            && self
                .modal_buffers
                .as_ref()
                .is_some_and(SurfaceBuffers::available)
        {
            let accounts = self.accounts();
            let account = &accounts[self.account_index.min(accounts.len() - 1)];
            let shortcut_labels: Vec<String> = SHORTCUTS
                .iter()
                .map(|s| {
                    if s.is_empty() {
                        "Sin atajo".into()
                    } else {
                        (*s).into()
                    }
                })
                .collect();
            let mic_labels = self.microphone_labels();
            let mon_labels = self.monitor_labels();
            let (options, selected) = match self.open_select {
                Some(0) => (accounts.clone(), self.account_index),
                Some(1) => (self.available(), self.model_index),
                Some(2) => (shortcut_labels.clone(), self.shortcut_index),
                Some(3) => (mic_labels.clone(), self.microphone_index),
                Some(4) => (mon_labels.clone(), self.monitor_index),
                _ => (Vec::new(), 0),
            };
            let pixels = crate::paint::modal(
                540,
                crate::paint::MODAL_HEIGHT,
                self.color,
                &Modal {
                    tab: self.tab,
                    account,
                    model: &self.draft.model,
                    shortcut: &shortcut_labels[self.shortcut_index],
                    microphone: &mic_labels[self.microphone_index.min(mic_labels.len() - 1)],
                    monitor: &mon_labels[self.monitor_index.min(mon_labels.len() - 1)],
                    open: self.open_select,
                    options: &options,
                    selected,
                    message: &self.message,
                    temperature: self.draft.temperature,
                    prompt: &self.draft.prompt,
                    prompt_cursor: self.prompt_cursor,
                    prompt_focused: self.prompt_focused,
                    base_url: &self.draft.base_url,
                    server_cursor: self.server_cursor,
                    server_focused: self.server_focused,
                },
            )?;
            if self
                .modal_buffers
                .as_mut()
                .unwrap()
                .attach(self.modal_surface.as_ref().unwrap(), &pixels)?
            {
                self.dirty = false;
            }
        }
        if !self.panel {
            self.dirty = false;
        }
        Ok(())
    }
    fn toggle_modal(&mut self, qh: &QueueHandle<Self>) {
        if self.panel {
            let restore_dock = self.draft.monitor != self.saved.monitor;
            self.discard_draft();
            if restore_dock {
                self.setup_dock(qh);
            } else if let (Some(layer), Some(surface)) = (&self.layer, &self.surface) {
                layer.set_margin(0, 0, 0, self.draft.x);
                surface.commit();
            }
            self.panel = false;
            self.open_select = None;
            self.prompt_focused = false;
            if let Some(layer) = self.modal_layer.take() {
                layer.destroy();
            }
            if let Some(surface) = self.modal_surface.take() {
                surface.destroy();
            }
            self.modal_configured = false;
            self.modal_buffers = None;
        } else {
            self.discard_draft();
            self.panel = true;
            self.tab = 0;
            self.prompt_cursor = self.draft.prompt.chars().count();
            self.prompt_focused = false;
            let surface = self.compositor.as_ref().unwrap().create_surface(qh, ());
            let layer = self.shell.as_ref().unwrap().get_layer_surface(
                &surface,
                None,
                Layer::Overlay,
                "hypr-dictate-modal".into(),
                qh,
                (),
            );
            layer.set_size(540, crate::paint::MODAL_HEIGHT as u32);
            layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            surface.commit();
            self.modal_surface = Some(surface);
            self.modal_layer = Some(layer);
        }
        self.dirty = true;
    }
    fn sync_model_indices(&mut self) {
        let account = self
            .draft
            .model
            .split_once('/')
            .map(|(a, _)| a.to_string())
            .unwrap_or_else(|| "Automática".into());
        self.account_index = self
            .accounts()
            .iter()
            .position(|a| a == &account)
            .unwrap_or(0);
        self.model_index = self
            .available()
            .iter()
            .position(|m| m == &self.draft.model)
            .unwrap_or(0);
    }
    pub fn catalog_loaded(&mut self) {
        self.sync_model_indices();
        self.dirty = true;
        self.message = format!(
            "{} rutas disponibles · elige cuenta, modelo, atajo, micro y pantalla",
            self.models.len()
        );
    }
    fn accounts(&self) -> Vec<String> {
        let mut out = vec!["Automática".into()];
        for model in &self.models {
            if let Some((account, _)) = model.split_once('/')
                && !out.iter().any(|a| a == account)
            {
                out.push(account.into());
            }
        }
        out
    }
    fn available(&self) -> Vec<String> {
        let accounts = self.accounts();
        let account = &accounts[self.account_index.min(accounts.len() - 1)];
        self.models
            .iter()
            .filter(|m| {
                if account == "Automática" {
                    !m.contains('/')
                } else {
                    m.starts_with(&format!("{account}/"))
                }
            })
            .cloned()
            .collect()
    }
    fn choose_model(&mut self, step: isize) {
        let models = self.available();
        if !models.is_empty() {
            self.model_index =
                (self.model_index as isize + step).rem_euclid(models.len() as isize) as usize;
            self.draft.model = models[self.model_index].clone();
        }
    }
    fn microphone_labels(&self) -> Vec<String> {
        let mut out = vec!["Automático".into()];
        out.extend(self.microphones.iter().map(|m| m.description.clone()));
        out
    }
    fn choose_microphone(&mut self, step: isize) {
        let len = self.microphones.len() + 1;
        self.microphone_index = (self.microphone_index as isize + step).rem_euclid(len as isize) as usize;
        self.draft.microphone = if self.microphone_index == 0 {
            String::new()
        } else {
            self.microphones[self.microphone_index - 1].name.clone()
        };
    }
    fn monitor_labels(&self) -> Vec<String> {
        let mut out = vec!["Automática (enfocada)".into()];
        out.extend(self.monitors.iter().map(|m| {
            if m.description.is_empty() || m.description == m.name {
                m.name.clone()
            } else {
                format!("{} ({})", m.name, m.description)
            }
        }));
        out
    }
    fn find_output(&self, monitor_name: &str) -> Option<&wl_output::WlOutput> {
        if monitor_name.is_empty() {
            return None;
        }
        self.outputs.iter().find(|o| o.name == monitor_name).map(|o| &o.output)
    }
    fn update_max_x(&mut self) {
        let width = if self.draft.monitor.is_empty() {
            self.monitors.iter().find(|m| m.focused).map(|m| m.width)
        } else {
            self.monitors.iter().find(|m| m.name == self.draft.monitor).map(|m| m.width)
        }
        .unwrap_or(1280);
        self.max_x = width.saturating_sub(176);
    }
    fn setup_dock(&mut self, qh: &QueueHandle<Self>) {
        self.dock_buffers = None;
        self.dock_dirty = true;
        if let Some(layer) = self.layer.take() {
            layer.destroy();
        }
        if let Some(surface) = self.surface.take() {
            surface.destroy();
        }
        self.configured = false;
        let surface = self.compositor.as_ref().unwrap().create_surface(qh, ());
        let target_output = self.find_output(&self.draft.monitor);
        let layer = self.shell.as_ref().unwrap().get_layer_surface(
            &surface,
            target_output,
            Layer::Overlay,
            "hypr-dictate".into(),
            qh,
            (),
        );
        layer.set_size(176, 44);
        layer.set_anchor(Anchor::Bottom | Anchor::Left);
        self.update_max_x();
        self.draft.x = self.draft.x.clamp(0, self.max_x);
        layer.set_margin(0, 0, 0, self.draft.x);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(0);
        surface.commit();
        self.surface = Some(surface);
        self.layer = Some(layer);
        self.dirty = true;
    }
    fn choose_monitor(&mut self, step: isize, qh: &QueueHandle<Self>) {
        let len = self.monitors.len() + 1;
        self.monitor_index = (self.monitor_index as isize + step).rem_euclid(len as isize) as usize;
        self.draft.monitor = if self.monitor_index == 0 {
            String::new()
        } else {
            self.monitors[self.monitor_index - 1].name.clone()
        };
        self.setup_dock(qh);
    }
    fn modal_click(&mut self, qh: &QueueHandle<Self>) {
        let (x, y) = self.pointer_xy;

        // Header buttons: Reload and Close
        if (12.0..38.0).contains(&y) {
            if (345.0..440.0).contains(&x) {
                let _ = self.tx.send(Event::Command("reload".into()));
                self.message = "Dock reiniciado".into();
                self.dirty = true;
                return;
            } else if x > 445.0 {
                self.toggle_modal(qh);
                return;
            }
        }

        // Tab selection buttons
        if (40.0..76.0).contains(&y) {
            if (26.0..130.0).contains(&x) {
                self.tab = 0;
                self.open_select = None;
                self.prompt_focused = false;
                self.server_focused = false;
                self.dirty = true;
                return;
            } else if (132.0..280.0).contains(&x) {
                self.tab = 1;
                self.open_select = None;
                self.server_focused = false;
                self.dirty = true;
                return;
            } else if (282.0..400.0).contains(&x) {
                self.tab = 2;
                self.open_select = None;
                self.prompt_focused = false;
                self.dirty = true;
                return;
            }
        }

        let actions_y = 412.;

        if self.tab == 2 {
            let (left, top, width, height) = crate::paint::SERVER_BOX;
            self.server_focused =
                (left..left + width).contains(&x) && (top..top + height).contains(&y);
            if self.server_focused {
                self.server_cursor = self.draft.base_url.chars().count();
            } else if (actions_y..actions_y + 40.).contains(&y) {
                if (26.0..206.0).contains(&x) {
                    let _ = self.tx.send(Event::SaveServer(self.draft.base_url.clone()));
                } else if (216.0..326.0).contains(&x) {
                    self.toggle_modal(qh);
                }
            }
            self.dirty = true;
            return;
        }
        if self.tab == 0 {
            if let Some(idx) = self.open_select {
                let count = match idx {
                    0 => self.accounts().len(),
                    1 => self.available().len(),
                    2 => SHORTCUTS.len(),
                    3 => self.microphones.len() + 1,
                    _ => self.monitors.len() + 1,
                };
                let visible = count.min(6);
                let h_list = visible as f64 * 30. + 10.;
                let base_y = if idx == 4 {
                    crate::paint::ROW_TOP[idx] - h_list + 6.
                } else {
                    crate::paint::ROW_TOP[idx] + 48.
                };
                let start = [
                    self.account_index,
                    self.model_index,
                    self.shortcut_index,
                    self.microphone_index,
                    self.monitor_index,
                ][idx]
                    .saturating_sub(2)
                    .min(count.saturating_sub(visible));
                if (26.0..514.0).contains(&x)
                    && (base_y..base_y + visible as f64 * 30. + 10.).contains(&y)
                {
                    let chosen = (start + ((y - base_y - 5.).max(0.) as usize / 30))
                        .min(count.saturating_sub(1));
                    match idx {
                        0 => {
                            self.account_index = chosen;
                            self.model_index = 0;
                            self.choose_model(0);
                        }
                        1 => {
                            self.model_index = chosen;
                            self.choose_model(0);
                        }
                        2 => {
                            self.shortcut_index = chosen;
                            self.draft.shortcut = SHORTCUTS[chosen].into();
                        }
                        3 => {
                            self.microphone_index = chosen;
                            self.choose_microphone(0);
                        }
                        _ => {
                            self.monitor_index = chosen;
                            self.choose_monitor(0, qh);
                        }
                    }
                    self.open_select = None;
                    return;
                }
            }
            for (idx, top) in crate::paint::ROW_TOP.map(|t| t + 8.).into_iter().enumerate() {
                if (26.0..514.0).contains(&x) && (top..top + 38.).contains(&y) {
                    self.open_select = if self.open_select == Some(idx) {
                        None
                    } else {
                        Some(idx)
                    };
                    return;
                }
            }
            self.open_select = None;

            if (actions_y..actions_y + 40.).contains(&y) {
                if (26.0..206.0).contains(&x) {
                    let _ = self.tx.send(Event::Save(self.draft.clone()));
                } else if (216.0..326.0).contains(&x) {
                    self.toggle_modal(qh);
                }
            }
        } else {
            // Tab 1: Parámetros e IA
            // Click on Temperature slider
            if (crate::paint::TEMP_BOX.1..crate::paint::TEMP_BOX.1 + crate::paint::TEMP_BOX.3).contains(&y)
                && (crate::paint::TEMP_BOX.0..crate::paint::TEMP_BOX.0 + crate::paint::TEMP_BOX.2).contains(&x)
            {
                let fraction = (x - crate::paint::TEMP_BOX.0) / crate::paint::TEMP_BOX.2;
                self.draft.temperature = ((fraction * 20.0).round() * 0.05).clamp(0.0, 1.0);
                self.dirty = true;
                return;
            }

            // Click inside prompt box
            if (crate::paint::PROMPT_BOX.1..crate::paint::PROMPT_BOX.1 + crate::paint::PROMPT_BOX.3).contains(&y)
                && (crate::paint::PROMPT_BOX.0..crate::paint::PROMPT_BOX.0 + crate::paint::PROMPT_BOX.2).contains(&x)
            {
                self.prompt_focused = true;
                let line_idx = ((y - crate::paint::PROMPT_BOX.1 - 10.0).max(0.0) / 18.0) as usize;
                let lines = crate::paint::wrap_prompt_lines(&self.draft.prompt, 54);
                if !lines.is_empty() {
                    let clamped_line = line_idx.min(lines.len() - 1);
                    let (start_char, line_str) = &lines[clamped_line];
                    let approx_char = ((x - crate::paint::PROMPT_BOX.0 - 12.0).max(0.0) / 7.2) as usize;
                    let line_len = line_str.chars().count();
                    self.prompt_cursor = start_char + approx_char.min(line_len);
                }
                self.dirty = true;
                return;
            }

            // Unfocus prompt if click is outside prompt box
            self.prompt_focused = false;

            if (actions_y..actions_y + 40.).contains(&y) {
                if (26.0..176.0).contains(&x) {
                    let _ = self.tx.send(Event::Save(self.draft.clone()));
                } else if (186.0..326.0).contains(&x) {
                    self.draft.prompt = crate::settings::DEFAULT_PROMPT.into();
                    self.draft.temperature = crate::settings::DEFAULT_TEMPERATURE;
                    self.prompt_cursor = self.draft.prompt.chars().count();
                    self.message = "Parámetros restaurados a base".into();
                    self.dirty = true;
                } else if (336.0..426.0).contains(&x) {
                    self.toggle_modal(qh);
                }
            }
        }
    }
    pub fn type_text(&mut self, text: &str, address: &str) -> Result<()> {
        let cleaned = hypr_dictate::sanitize_plain_text(text);
        if cleaned.is_empty() {
            return Ok(());
        }
        // Wait for physical modifiers (CTRL/ALT/SHIFT/SUPER) to be released so the target app
        // never sees Ctrl+key or Alt+key combinations while typing.
        for _ in 0..100 {
            if !crate::shortcut::any_modifier_held() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        let keyboard = self.keyboard.as_ref().unwrap();
        let all_chars: Vec<char> = cleaned.chars().collect();
        let mut cursor = 0;
        while cursor < all_chars.len() {
            ensure!(
                crate::active_address()? == address,
                "Cambió el foco durante la inserción; inserción detenida"
            );
            let mut map = BTreeMap::new();
            let mut names = String::new();
            let mut symbols = String::new();
            let mut end = cursor;
            while end < all_chars.len() {
                let ch = all_chars[end];
                if !map.contains_key(&ch) {
                    if map.len() == hypr_dictate::SAFE_EVDEV_KEYS.len() {
                        break;
                    }
                    let slot = map.len();
                    let evdev = hypr_dictate::SAFE_EVDEV_KEYS[slot];
                    let xkb = evdev + 8;
                    map.insert(ch, evdev);
                    names.push_str(&format!("<K{slot}> = {xkb};\n"));
                    symbols.push_str(&format!("key <K{slot}> {{ [ U{:04X} ] }};\n", ch as u32));
                }
                end += 1;
            }
            let keymap = format!(
                "xkb_keymap {{ xkb_keycodes \"dictate\" {{ minimum = 8; maximum = 255; {names} }}; xkb_types \"dictate\" {{ type \"ONE_LEVEL\" {{ modifiers = None; level_name[Level1] = \"Any\"; }}; }}; xkb_compatibility \"dictate\" {{}}; xkb_symbols \"dictate\" {{ {symbols} }}; }};\0"
            );
            let mut file = memory_file()?;
            file.write_all(keymap.as_bytes())?;
            file.seek(SeekFrom::Start(0))?;
            keyboard.keymap(1, file.as_fd(), keymap.len() as u32);
            keyboard.modifiers(0, 0, 0, 0);
            self.connection.flush()?;
            std::thread::sleep(Duration::from_millis(6));
            for ch in &all_chars[cursor..end] {
                let code = map[ch];
                keyboard.key(0, code, 1);
                keyboard.key(0, code, 0);
            }
            self.connection.flush()?;
            std::thread::sleep(Duration::from_millis(8));
            cursor = end;
        }
        Ok(())
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for Ui {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(7), qh, ()))
                }
                "zwlr_layer_shell_v1" => {
                    state.shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_output" => {
                    let output: wl_output::WlOutput = registry.bind(name, version.min(4), qh, ());
                    state.outputs.push(OutputItem {
                        name: String::new(),
                        output,
                    });
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for Ui {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Keyboard) && state.seat_keyboard.is_none() {
                state.seat_keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for Ui {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::WEnum::Value;
        match event {
            wl_keyboard::Event::Keymap { format: _, fd, size } => {
                let keymap = unsafe {
                    xkb::Keymap::new_from_fd(
                        &state.xkb_context,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                };
                if let Ok(Some(keymap)) = keymap {
                    state.xkb_state = Some(xkb::State::new(&keymap));
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb_state) = &mut state.xkb_state {
                    xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: Value(wl_keyboard::KeyState::Pressed),
                ..
            } if state.panel && state.tab == 2 && state.server_focused => {
                let xkb_key = xkb::Keycode::new(key + 8);
                if let Some(xkb_state) = &state.xkb_state {
                    let sym = xkb_state.key_get_one_sym(xkb_key);
                    let text = xkb_state.key_get_utf8(xkb_key);
                    state.edit_server(sym, &text);
                }
            }
            wl_keyboard::Event::Key {
                key,
                state: Value(wl_keyboard::KeyState::Pressed),
                ..
            } if state.panel && state.tab == 1 && state.prompt_focused => {
                let xkb_key = xkb::Keycode::new(key + 8);
                if let Some(xkb_state) = &state.xkb_state {
                    let sym = xkb_state.key_get_one_sym(xkb_key);
                    let utf8 = xkb_state.key_get_utf8(xkb_key);

                    let prompt_chars: Vec<char> = state.draft.prompt.chars().collect();
                    let total_chars = prompt_chars.len();
                    state.prompt_cursor = state.prompt_cursor.min(total_chars);

                    match sym {
                        xkb::Keysym::BackSpace => {
                            if state.prompt_cursor > 0 {
                                state.prompt_cursor -= 1;
                                let mut new_prompt =
                                    String::with_capacity(state.draft.prompt.len());
                                for (i, ch) in prompt_chars.iter().enumerate() {
                                    if i != state.prompt_cursor {
                                        new_prompt.push(*ch);
                                    }
                                }
                                state.draft.prompt = new_prompt;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Delete => {
                            if state.prompt_cursor < total_chars {
                                let mut new_prompt =
                                    String::with_capacity(state.draft.prompt.len());
                                for (i, ch) in prompt_chars.iter().enumerate() {
                                    if i != state.prompt_cursor {
                                        new_prompt.push(*ch);
                                    }
                                }
                                state.draft.prompt = new_prompt;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Left => {
                            if state.prompt_cursor > 0 {
                                state.prompt_cursor -= 1;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Right => {
                            if state.prompt_cursor < total_chars {
                                state.prompt_cursor += 1;
                                state.dirty = true;
                            }
                        }
                        xkb::Keysym::Home => {
                            state.prompt_cursor = 0;
                            state.dirty = true;
                        }
                        xkb::Keysym::End => {
                            state.prompt_cursor = total_chars;
                            state.dirty = true;
                        }
                        xkb::Keysym::Return => {
                            let mut new_prompt =
                                String::with_capacity(state.draft.prompt.len() + 1);
                            for (i, ch) in prompt_chars.iter().enumerate() {
                                if i == state.prompt_cursor {
                                    new_prompt.push(' ');
                                }
                                new_prompt.push(*ch);
                            }
                            if state.prompt_cursor == total_chars {
                                new_prompt.push(' ');
                            }
                            state.prompt_cursor += 1;
                            state.draft.prompt = new_prompt;
                            state.dirty = true;
                        }
                        _ => {
                            if !utf8.is_empty() && !utf8.chars().any(char::is_control) {
                                let inserted_count = utf8.chars().count();
                                let mut new_prompt = String::with_capacity(
                                    state.draft.prompt.len() + utf8.len(),
                                );
                                let mut inserted = false;
                                for (i, ch) in prompt_chars.iter().enumerate() {
                                    if i == state.prompt_cursor {
                                        new_prompt.push_str(&utf8);
                                        inserted = true;
                                    }
                                    new_prompt.push(*ch);
                                }
                                if !inserted {
                                    new_prompt.push_str(&utf8);
                                }
                                state.prompt_cursor += inserted_count;
                                state.draft.prompt = new_prompt;
                                state.dirty = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if state.dirty && let Err(error) = state.draw(qh) {
            state.message = error.to_string();
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for Ui {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::WEnum::Value;
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.in_modal = state.modal_surface.as_ref() == Some(&surface);
                state.pointer_xy = (surface_x, surface_y);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_xy = (surface_x, surface_y);
                if !state.in_modal
                    && let Some((x, _, origin)) = state.press
                {
                    let delta = surface_x - x;
                    let origin = if state.dragged { state.draft.x } else { origin };
                    if delta.abs() > 6. || state.dragged {
                        state.dragged = true;
                        state.draft.x = (origin + delta as i32).clamp(0, state.max_x);
                        state
                            .layer
                            .as_ref()
                            .unwrap()
                            .set_margin(0, 0, 0, state.draft.x);
                        state.surface.as_ref().unwrap().commit();
                    }
                }
            }
            wl_pointer::Event::Axis { value, .. } if state.in_modal => {
                let step = if value > 0. { 1 } else { -1 };
                if state.tab == 1 {
                    if (crate::paint::TEMP_BOX.1 - 10.0..crate::paint::TEMP_BOX.1 + crate::paint::TEMP_BOX.3 + 10.0)
                        .contains(&state.pointer_xy.1)
                    {
                        let delta = if value > 0. { -0.05 } else { 0.05 };
                        state.draft.temperature =
                            ((state.draft.temperature + delta) * 100.0).round() / 100.0;
                        state.draft.temperature = state.draft.temperature.clamp(0.0, 1.0);
                        state.dirty = true;
                    }
                } else if state.tab == 0 {
                    match state.open_select {
                        Some(0) => {
                            let len = state.accounts().len() as isize;
                            state.account_index =
                                (state.account_index as isize + step).rem_euclid(len) as usize;
                            state.model_index = 0;
                            state.choose_model(0);
                        }
                        Some(2) => {
                            state.shortcut_index = (state.shortcut_index as isize + step)
                                .rem_euclid(SHORTCUTS.len() as isize)
                                as usize;
                            state.draft.shortcut = SHORTCUTS[state.shortcut_index].into();
                        }
                        Some(3) => state.choose_microphone(step),
                        Some(4) => state.choose_monitor(step, qh),
                        _ => state.choose_model(step),
                    }
                    state.dirty = true;
                }
            }
            wl_pointer::Event::Button {
                button: 273,
                state: Value(wl_pointer::ButtonState::Pressed),
                ..
            } => state.toggle_modal(qh),
            wl_pointer::Event::Button {
                button: 272,
                state: Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                state.press = Some((state.pointer_xy.0, state.pointer_xy.1, state.draft.x));
                state.dragged = false;
            }
            wl_pointer::Event::Button {
                button: 272,
                state: Value(wl_pointer::ButtonState::Released),
                ..
            } if state.press.take().is_some() => {
                if state.in_modal {
                    state.modal_click(qh);
                } else if state.dragged {
                    state.save_position();
                } else {
                    let _ = state.tx.send(Event::Command("toggle".into()));
                }
                state.dirty = true;
            }
            _ => {}
        }
        if state.dirty
            && let Err(error) = state.draw(qh)
        {
            state.message = error.to_string();
        }
    }
}
impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Ui {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                if state.modal_layer.as_ref() == Some(layer) {
                    state.modal_configured = true;
                } else {
                    state.configured = true;
                    state.dock_dirty = true;
                }
                state.dirty = true;
                if let Err(error) = state.draw(qh) {
                    eprintln!("No se pudo dibujar: {error}");
                    state.closed = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if state.modal_layer.as_ref() == Some(layer) {
                    state.toggle_modal(qh);
                } else {
                    state.closed = true;
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for Ui {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            if let Some(buffers) = &mut state.dock_buffers {
                buffers.release(buffer);
            }
            if let Some(buffers) = &mut state.modal_buffers {
                buffers.release(buffer);
            }
        }
    }
}
impl Dispatch<wl_output::WlOutput, ()> for Ui {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(item) = state.outputs.iter_mut().find(|o| &o.output == output)
        {
            item.name = name;
        }
    }
}
delegate_noop!(Ui: ignore wl_compositor::WlCompositor);
delegate_noop!(Ui: ignore wl_surface::WlSurface);
delegate_noop!(Ui: ignore wl_shm::WlShm);
delegate_noop!(Ui: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Ui: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(Ui: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(Ui: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);

fn monitor_width() -> Option<i32> {
    let value: serde_json::Value =
        serde_json::from_slice(&crate::hypr(&["-j", "monitors"]).ok()?).ok()?;
    let monitor = value.as_array()?.iter().find(|m| m["focused"] == true)?;
    Some((monitor["width"].as_f64()? / monitor["scale"].as_f64().unwrap_or(1.)) as i32)
}
pub fn run(mut app: App, rx: mpsc::Receiver<Event>) -> Result<()> {
    let connection = Connection::connect_to_env().context("No se pudo conectar con Wayland")?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    // Microphone detection is a cheap local pw-dump call, unlike the model
    // catalog which needs a network round trip, so it is resolved up front
    // and never blocks startup if PipeWire's CLI tooling has an issue.
    let microphones = crate::settings::microphones().unwrap_or_default();
    let monitors = crate::settings::monitors();
    let mut ui = Ui::new(
        connection.clone(),
        &app,
        microphones,
        monitors,
        monitor_width().unwrap_or(1280).saturating_sub(176),
    );
    connection.display().get_registry(&qh, ());
    queue.roundtrip(&mut ui)?;
    queue.roundtrip(&mut ui)?;
    ensure!(
        ui.compositor.is_some()
            && ui.shm.is_some()
            && ui.seat.is_some()
            && ui.shell.is_some()
            && ui.manager.is_some(),
        "El compositor no ofrece los protocolos Wayland necesarios"
    );
    ui.keyboard = Some(ui.manager.as_ref().unwrap().create_virtual_keyboard(
        ui.seat.as_ref().unwrap(),
        &qh,
        (),
    ));
    ui.setup_dock(&qh);
    queue.roundtrip(&mut ui)?;
    while !ui.closed {
        queue.dispatch_pending(&mut ui)?;
        while let Ok(event) = rx.try_recv() {
            app.event(event, &mut ui);
            ui.dirty = true;
        }
        let color = app.color();
        let active = matches!(
            app.phase,
            hypr_dictate::Phase::Recording(_) | hypr_dictate::Phase::Processing(_)
        );
        let target = if active { 1. } else { 0. };
        if (ui.open_progress - target).abs() > 0.01 {
            ui.open_progress = (ui.open_progress
                + if target > ui.open_progress {
                    0.14
                } else {
                    -0.1
                })
            .clamp(0., 1.);
            ui.dock_dirty = true;
        }
        let animating = active || ui.open_progress > 0.01;
        if matches!(app.phase, hypr_dictate::Phase::Processing(_)) {
            ui.level = -1.;
        } else {
            let peak = app.level.load(std::sync::atomic::Ordering::Relaxed) as f64;
            // Clean noise floor filter: ignore quiet room ambient noise under 100
            // and use square root scaling for vibrant, responsive voice dynamics
            let normalized = ((peak - 100.).max(0.) / 2400.).min(1.);
            let raw = normalized.sqrt();
            if raw > ui.level {
                ui.level = raw;
            } else {
                ui.level = (ui.level * 0.86 - 0.008).max(0.);
            }
        }
        if animating || color != ui.color {
            ui.animate_dock(color);
        }
        if (ui.dock_dirty || ui.dirty) && ui.configured {
            ui.draw(&qh)?;
        }
        connection.flush()?;
        if let Some(guard) = queue.prepare_read() {
            let mut fd = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&connection.backend().poll_fd()),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut fd, 1, if animating { 16 } else { 100 }) };
            if ready > 0 {
                if let Err(error) = guard.read()
                    && !hypr_dictate::transient_wayland_error(&error)
                {
                    return Err(error.into());
                }
            } else {
                drop(guard);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Microphone, MonitorInfo};
    use wayland_client::Proxy;

    fn ui() -> (Ui, std::os::unix::net::UnixStream) {
        let (socket, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let mut app = crate::tests::app(hypr_dictate::Phase::Idle);
        app.settings = crate::settings::Settings {
            model: "account/model".into(),
            shortcut: "SUPER + F9".into(),
            microphone: "saved-mic".into(),
            monitor: "saved-monitor".into(),
            ..Default::default()
        };
        let mut ui = Ui::new(
            connection,
            &app,
            vec![Microphone {
                name: "saved-mic".into(),
                description: "Mic".into(),
            }],
            vec![MonitorInfo {
                name: "saved-monitor".into(),
                description: "Monitor".into(),
                width: 1920,
                focused: true,
            }],
            1744,
        );
        ui.models = vec!["account/model".into()];
        (ui, server)
    }

    fn shm_and_surface(ui: &Ui) -> (wl_shm::WlShm, wl_surface::WlSurface, QueueHandle<Ui>) {
        let queue = ui.connection.new_event_queue::<Ui>();
        let qh = queue.handle();
        let registry = ui.connection.display().get_registry(&qh, ());
        let shm = registry.bind::<wl_shm::WlShm, _, _>(1, 1, &qh, ());
        let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(2, 4, &qh, ());
        (shm, compositor.create_surface(&qh, ()), qh)
    }

    #[test]
    fn shared_files_are_memory_backed_and_close_on_exec() {
        use std::os::fd::AsRawFd;
        let file = memory_file().unwrap();
        let target = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
        assert!(
            target.to_string_lossy().starts_with("/memfd:"),
            "{target:?}"
        );
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn surface_reuses_only_released_buffers_and_never_overwrites_busy_pixels() {
        let (ui, _server) = ui();
        let (shm, surface, qh) = shm_and_surface(&ui);
        let mut buffers = SurfaceBuffers::new(&shm, 2, 2, &qh).unwrap();
        let ids = buffers.buffers.each_ref().map(|buffer| buffer.id());
        assert!(buffers.attach(&surface, &[1; 16]).unwrap());
        assert!(buffers.attach(&surface, &[2; 16]).unwrap());
        assert!(!buffers.available());
        assert!(!buffers.attach(&surface, &[3; 16]).unwrap());
        let mut pixels = [0; 32];
        buffers.file.read_exact_at(&mut pixels, 0).unwrap();
        assert_eq!(&pixels[..16], &[1; 16]);
        assert_eq!(&pixels[16..], &[2; 16]);
        let first = buffers.buffers[0].clone();
        buffers.release(&first);
        assert!(buffers.available());
        assert!(buffers.attach(&surface, &[3; 16]).unwrap());
        buffers.file.read_exact_at(&mut pixels, 0).unwrap();
        assert_eq!(&pixels[..16], &[3; 16]);
        assert_eq!(&pixels[16..], &[2; 16]);
        assert_eq!(buffers.buffers.each_ref().map(|buffer| buffer.id()), ids);
        assert_eq!(buffers.file.metadata().unwrap().len(), 32);
    }

    #[test]
    fn unchanged_modal_reuses_its_frame_while_dock_animates() {
        let (mut ui, _server) = ui();
        let (shm, dock, qh) = shm_and_surface(&ui);
        let (_, modal, _) = shm_and_surface(&ui);
        ui.shm = Some(shm);
        ui.surface = Some(dock);
        ui.modal_surface = Some(modal);
        ui.configured = true;
        ui.modal_configured = true;
        ui.panel = true;
        ui.draw(&qh).unwrap();
        let modal_ids = ui
            .modal_buffers
            .as_ref()
            .unwrap()
            .buffers
            .each_ref()
            .map(|b| b.id());
        let color = ui.color;
        for _ in 0..60 {
            let buffer = ui.dock_buffers.as_ref().unwrap().buffers[0].clone();
            let connection = ui.connection.clone();
            <Ui as Dispatch<wl_buffer::WlBuffer, ()>>::event(
                &mut ui,
                &buffer,
                wl_buffer::Event::Release,
                &(),
                &connection,
                &qh,
            );
            ui.animate_dock(color);
            ui.draw(&qh).unwrap();
            assert!(!ui.dirty);
            assert!(!ui.dock_dirty);
            let modal = ui.modal_buffers.as_ref().unwrap();
            assert_eq!(modal.busy, [true, false]);
            assert_eq!(modal.buffers.each_ref().map(|b| b.id()), modal_ids);
        }
    }

    #[test]
    fn modal_update_waits_for_release_without_losing_dirty_state() {
        let (mut ui, _server) = ui();
        let (shm, modal, qh) = shm_and_surface(&ui);
        ui.shm = Some(shm);
        ui.modal_surface = Some(modal);
        ui.panel = true;
        ui.modal_configured = true;
        ui.draw(&qh).unwrap();
        ui.message = "updated".into();
        ui.dirty = true;
        ui.draw(&qh).unwrap();
        ui.message = "latest update".into();
        ui.dirty = true;
        ui.draw(&qh).unwrap();
        assert!(ui.dirty);
        let first = ui.modal_buffers.as_ref().unwrap().buffers[0].clone();
        let connection = ui.connection.clone();
        <Ui as Dispatch<wl_buffer::WlBuffer, ()>>::event(
            &mut ui,
            &first,
            wl_buffer::Event::Release,
            &(),
            &connection,
            &qh,
        );
        ui.draw(&qh).unwrap();
        assert!(!ui.dirty);
        assert_eq!(ui.modal_buffers.as_ref().unwrap().busy, [true, true]);
    }

    #[test]
    fn invalid_frames_and_old_releases_cannot_corrupt_a_pool() {
        let (ui, _server) = ui();
        let (shm, surface, qh) = shm_and_surface(&ui);
        assert!(SurfaceBuffers::new(&shm, 0, 2, &qh).is_err());
        assert!(SurfaceBuffers::new(&shm, i32::MAX, 2, &qh).is_err());
        let old = SurfaceBuffers::new(&shm, 2, 2, &qh).unwrap();
        let old_buffer = old.buffers[0].clone();
        let mut new = SurfaceBuffers::new(&shm, 2, 2, &qh).unwrap();
        new.attach(&surface, &[7; 16]).unwrap();
        assert!(new.attach(&surface, &[0; 15]).is_err());
        new.release(&old_buffer);
        assert_eq!(new.busy, [true, false]);
        let mut pixels = [0; 16];
        new.file.read_exact_at(&mut pixels, 0).unwrap();
        assert_eq!(pixels, [7; 16]);
    }

    #[test]
    fn dock_animation_does_not_invalidate_unchanged_modal() {
        let (mut ui, _server) = ui();
        ui.dirty = false;
        ui.dock_dirty = false;
        let color = ui.color;
        for _ in 0..60 {
            ui.animate_dock(color);
            assert!(!ui.dirty, "dock tick must not invalidate the modal");
            assert!(ui.dock_dirty);
        }
        ui.animate_dock([235, 65, 75]);
        assert!(ui.dirty, "phase color change must update modal styling");
    }

    #[test]
    fn discarded_draft_restores_all_settings_and_selector_indices() {
        let (mut ui, _server) = ui();
        let expected = format!("{:?}", ui.saved);
        ui.draft = crate::settings::Settings {
            model: "other/model".into(),
            base_url: "https://unsaved.example".into(),
            x: 1,
            shortcut: "CTRL + ALT + D".into(),
            microphone: "other-mic".into(),
            monitor: "other-monitor".into(),
            prompt: "unsaved prompt".into(),
            temperature: 0.9,
        };
        ui.account_index = 99;
        ui.model_index = 99;
        ui.shortcut_index = 99;
        ui.microphone_index = 99;
        ui.monitor_index = 99;
        ui.prompt_cursor = 9999;
        ui.open_select = Some(2);
        ui.prompt_focused = true;
        ui.discard_draft();
        assert_eq!(format!("{:?}", ui.draft), expected);
        assert_eq!(ui.account_index, 1);
        assert_eq!(ui.model_index, 0);
        assert_eq!(ui.shortcut_index, 4);
        assert_eq!(ui.microphone_index, 1);
        assert_eq!(ui.monitor_index, 1);
        assert_eq!(ui.prompt_cursor, ui.saved.prompt.chars().count());
        assert_eq!(ui.open_select, None);
        assert!(!ui.prompt_focused);
    }

    #[test]
    fn closing_modal_discards_edits_without_emitting_a_save() {
        let (mut ui, _server) = ui();
        let (tx, rx) = mpsc::channel();
        ui.tx = tx;
        ui.panel = true;
        ui.draft.prompt = "unsaved prompt".into();
        ui.draft.shortcut = "CTRL + ALT + D".into();
        let queue = ui.connection.new_event_queue::<Ui>();
        ui.toggle_modal(&queue.handle());
        assert!(!ui.panel);
        assert_eq!(format!("{:?}", ui.draft), format!("{:?}", ui.saved));
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn every_close_button_discards_without_saving() {
        for (tab, pointer_xy) in [
            (0, (460., 20.)),
            (0, (230., 420.)),
            (1, (350., 420.)),
            (2, (230., 420.)),
        ] {
            let (mut ui, _server) = ui();
            let (tx, rx) = mpsc::channel();
            ui.tx = tx;
            ui.panel = true;
            ui.tab = tab;
            ui.pointer_xy = pointer_xy;
            ui.draft.prompt = "unsaved prompt".into();
            ui.draft.shortcut = "CTRL + ALT + D".into();
            let queue = ui.connection.new_event_queue::<Ui>();
            ui.modal_click(&queue.handle());
            assert!(!ui.panel);
            assert_eq!(format!("{:?}", ui.draft), format!("{:?}", ui.saved));
            assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        }
    }

    #[test]
    fn dragging_emits_only_position_even_without_catalog() {
        let (mut ui, _server) = ui();
        let (tx, rx) = mpsc::channel();
        ui.tx = tx;
        ui.models.clear();
        ui.draft.x = 456;
        ui.draft.shortcut = "CTRL + ALT + D".into();
        ui.draft.model = "unsaved/model".into();
        ui.draft.prompt = "unsaved prompt".into();
        let draft = format!("{:?}", ui.draft);
        ui.save_position();
        assert!(matches!(rx.try_recv().unwrap(), Event::SavePosition(456)));
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(format!("{:?}", ui.draft), draft);
    }

    #[test]
    fn saved_position_survives_discard_without_committing_other_edits() {
        let (mut ui, _server) = ui();
        let original_shortcut = ui.saved.shortcut.clone();
        ui.draft.shortcut = "CTRL + ALT + D".into();
        ui.position_saved(456);
        assert_eq!(ui.draft.shortcut, "CTRL + ALT + D");
        assert_eq!(ui.saved.shortcut, original_shortcut);
        ui.models.clear();
        ui.discard_draft();
        assert_eq!(ui.draft.x, 456);
        assert_eq!(ui.draft.shortcut, original_shortcut);
        assert_eq!(ui.account_index, 0);
        assert_eq!(ui.model_index, 0);
    }

    #[test]
    fn server_tab_focus_and_save_emit_only_the_url() {
        let (mut ui, _server) = ui();
        let (tx, rx) = mpsc::channel();
        ui.tx = tx;
        ui.panel = true;
        let queue = ui.connection.new_event_queue::<Ui>();
        ui.pointer_xy = (300., 60.);
        ui.modal_click(&queue.handle());
        assert_eq!(ui.tab, 2);
        ui.pointer_xy = (100., 125.);
        ui.modal_click(&queue.handle());
        assert!(ui.server_focused);
        ui.draft.base_url = "https://proxy.example".into();
        ui.draft.shortcut = "unsaved shortcut".into();
        ui.pointer_xy = (80., 425.);
        ui.modal_click(&queue.handle());
        assert!(
            matches!(rx.try_recv().unwrap(), Event::SaveServer(url) if url == "https://proxy.example")
        );
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(ui.saved.base_url, crate::settings::DEFAULT_BASE_URL);
    }

    #[test]
    fn server_save_updates_snapshot_without_committing_other_edits_and_clears_catalog() {
        let (mut ui, _server) = ui();
        let original_shortcut = ui.saved.shortcut.clone();
        ui.draft.shortcut = "unsaved shortcut".into();
        ui.server_saved("https://proxy.example");
        assert!(ui.models.is_empty());
        assert_eq!(ui.draft.shortcut, "unsaved shortcut");
        assert_eq!(ui.saved.shortcut, original_shortcut);
        ui.discard_draft();
        assert_eq!(ui.draft.base_url, "https://proxy.example");
        assert_eq!(ui.draft.shortcut, original_shortcut);
    }

    #[test]
    fn stale_catalog_success_and_failure_do_not_change_ui_or_phase() {
        let (mut ui, _server) = ui();
        let mut app = crate::tests::app(hypr_dictate::Phase::Idle);
        app.catalog_generation = 3;
        let models = ui.models.clone();
        let message = ui.message.clone();
        for result in [
            Ok(vec!["old/model".into()]),
            Err(anyhow::anyhow!("old failure")),
        ] {
            app.event(
                Event::Catalog(app.settings.base_url.clone(), 1, result),
                &mut ui,
            );
            assert_eq!(ui.models, models);
            assert_eq!(ui.message, message);
            assert_eq!(app.phase, hypr_dictate::Phase::Idle);
        }
        app.event(
            Event::Catalog(
                app.settings.base_url.clone(),
                3,
                Ok(vec!["new/model".into()]),
            ),
            &mut ui,
        );
        assert_eq!(ui.models, ["new/model"]);
    }

    #[test]
    fn server_editor_changes_only_draft_until_saved_or_discarded() {
        let (mut ui, _server) = ui();
        let original = ui.saved.base_url.clone();
        ui.draft.base_url = "http://127.0.0.1:8317".into();
        ui.server_cursor = ui.draft.base_url.chars().count();
        ui.edit_server(xkb::Keysym::End, "");
        ui.edit_server(xkb::Keysym::BackSpace, "");
        ui.edit_server(xkb::Keysym::NoSymbol, "8");
        assert_eq!(ui.draft.base_url, "http://127.0.0.1:8318");
        assert_eq!(ui.saved.base_url, original);
        ui.edit_server(xkb::Keysym::Home, "");
        ui.edit_server(xkb::Keysym::Delete, "");
        assert_eq!(ui.draft.base_url, "ttp://127.0.0.1:8318");
        let before = ui.draft.base_url.clone();
        ui.edit_server(xkb::Keysym::Return, "\r");
        ui.edit_server(xkb::Keysym::NoSymbol, "\n\t");
        assert_eq!(ui.draft.base_url, before);
        ui.server_focused = true;
        ui.discard_draft();
        assert_eq!(ui.draft.base_url, original);
        assert!(!ui.server_focused);
    }

    #[test]
    fn discard_uses_latest_successful_save_not_startup_settings() {
        let (mut ui, _server) = ui();
        let mut saved = ui.saved.clone();
        saved.prompt = "new saved prompt".into();
        saved.temperature = 0.6;
        ui.settings_saved(&saved);
        ui.draft.prompt = "later unsaved prompt".into();
        ui.discard_draft();
        assert_eq!(format!("{:?}", ui.draft), format!("{saved:?}"));
    }
}
