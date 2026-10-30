use crate::{App, Event, preferences::Preferences};
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
        wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
        wl_surface,
    },
};
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
    pointer: Option<wl_pointer::WlPointer>,
    tx: mpsc::Sender<Event>,
    configured: bool,
    closed: bool,
    color: [u8; 3],
    pub models: Vec<String>,
    pub message: String,
    draft: crate::settings::Settings,
    saved: crate::settings::Settings,
    preferences: Option<Preferences>,
    dock_reconfigure: bool,
    pointer_xy: (f64, f64),
    press: Option<(f64, f64, i32)>,
    dragged: bool,
    tick: f64,
    open_progress: f64,
    pub level: f64,
    pub microphones: Vec<crate::settings::Microphone>,
    pub monitors: Vec<crate::settings::MonitorInfo>,
    outputs: Vec<OutputItem>,
    dock_buffers: Option<SurfaceBuffers>,
    dock_dirty: bool,
    pub dirty: bool,
    max_x: i32,
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
            pointer: None,
            tx: app.tx.clone(),
            configured: false,
            closed: false,
            color: app.color(),
            models: Vec::new(),
            message: "Cargando catálogo…".into(),
            draft: app.settings.clone(),
            saved: app.settings.clone(),
            preferences: None,
            dock_reconfigure: false,
            pointer_xy: (0., 0.),
            press: None,
            dragged: false,
            tick: 0.,
            open_progress: 0.,
            level: 0.,
            microphones,
            monitors,
            outputs: Vec::new(),
            dock_buffers: None,
            dock_dirty: true,
            dirty: true,
            max_x,
        }
    }
    pub fn settings_saved(&mut self, settings: &crate::settings::Settings) {
        self.dock_reconfigure = self.saved.monitor != settings.monitor;
        self.saved = settings.clone();
        self.draft = settings.clone();
    }
    pub fn server_saved(&mut self, base_url: &str) {
        self.saved.base_url = base_url.into();
        self.draft.base_url = base_url.into();
        self.models.clear();
        if let Some(preferences) = &self.preferences {
            preferences.server_saved(base_url);
        }
        self.message = "Servidor guardado · cargando catálogo…".into();
        self.dirty = true;
    }
    pub fn position_saved(&mut self, x: i32) {
        self.saved.x = x;
        self.draft.x = x;
        if let Some(preferences) = &self.preferences {
            preferences.position_saved(x);
        }
    }
    fn save_position(&self) {
        let _ = self.tx.send(Event::SavePosition(self.draft.x));
    }
    fn animate_dock(&mut self, color: [u8; 3]) {
        self.color = color;
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
        if let Some(preferences) = &self.preferences {
            preferences.set_message(&self.message);
        }
        self.dirty = false;
        Ok(())
    }
    fn toggle_preferences(&mut self) {
        if let Some(preferences) = self.preferences.take()
            && preferences.visible()
        {
            preferences.close();
            return;
        }
        match Preferences::new(
            &self.saved,
            &self.models,
            &self.microphones,
            &self.monitors,
            &self.message,
            self.tx.clone(),
        ) {
            Ok(preferences) => {
                preferences.present();
                self.preferences = Some(preferences);
            }
            Err(error) => {
                self.message = format!("No se pudieron abrir los ajustes GTK4: {error}");
                eprintln!("{}", self.message);
            }
        }
    }
    pub fn catalog_loaded(&mut self) {
        if let Some(preferences) = &self.preferences {
            preferences.catalog_loaded(&self.models);
        }
        self.message = format!(
            "{} rutas disponibles · selecciona un modelo compatible con audio",
            self.models.len()
        );
        self.dirty = true;
    }
    fn find_output(&self, monitor_name: &str) -> Option<&wl_output::WlOutput> {
        if monitor_name.is_empty() {
            return None;
        }
        self.outputs
            .iter()
            .find(|o| o.name == monitor_name)
            .map(|o| &o.output)
    }
    fn update_max_x(&mut self) {
        let width = if self.draft.monitor.is_empty() {
            self.monitors.iter().find(|m| m.focused).map(|m| m.width)
        } else {
            self.monitors
                .iter()
                .find(|m| m.name == self.draft.monitor)
                .map(|m| m.width)
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
            && caps.contains(wl_seat::Capability::Pointer)
            && state.pointer.is_none()
        {
            state.pointer = Some(seat.get_pointer(qh, ()));
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
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_xy = (surface_x, surface_y);
            }
            wl_pointer::Event::Leave { .. } => {
                state.press = None;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_xy = (surface_x, surface_y);
                if let Some((x, _, origin)) = state.press {
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
            wl_pointer::Event::Button {
                button: 273,
                state: Value(wl_pointer::ButtonState::Pressed),
                ..
            } => state.toggle_preferences(),
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
                if state.dragged {
                    state.save_position();
                } else {
                    let _ = state.tx.send(Event::Command("toggle".into()));
                }
            }
            _ => {}
        }
        if (state.dirty || state.dock_dirty)
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
                state.configured = true;
                state.dock_dirty = true;
                if let Err(error) = state.draw(qh) {
                    eprintln!("No se pudo dibujar: {error}");
                    state.closed = true;
                }
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
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
        if let wl_buffer::Event::Release = event
            && let Some(buffers) = &mut state.dock_buffers
        {
            buffers.release(buffer);
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
        let context = gtk4::glib::MainContext::default();
        for _ in 0..64 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        if ui.preferences.as_ref().is_some_and(|p| !p.visible()) {
            ui.preferences = None;
        }
        while let Ok(event) = rx.try_recv() {
            app.event(event, &mut ui);
            ui.dirty = true;
        }
        if ui.dock_reconfigure {
            ui.setup_dock(&qh);
            ui.dock_reconfigure = false;
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
            let timeout = if animating || ui.preferences.is_some() {
                8
            } else {
                100
            };
            let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
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
    fn settings_apply_reconfigures_dock_only_after_success_and_preserves_server() {
        let (mut ui, _server) = ui();
        let mut settings = ui.saved.clone();
        settings.monitor = "other".into();
        ui.settings_saved(&settings);
        assert!(ui.dock_reconfigure);
        assert_eq!(ui.saved.monitor, "other");
        ui.server_saved("https://new.example");
        assert_eq!(ui.saved.monitor, "other");
        assert_eq!(ui.saved.base_url, "https://new.example");
        assert!(ui.models.is_empty());
    }
}
