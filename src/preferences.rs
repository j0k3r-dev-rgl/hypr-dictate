use crate::{
    Event,
    settings::{self, Microphone, MonitorInfo, Settings},
};
use anyhow::Result;
use gtk4::{self as gtk, prelude::*};
use hypr_dictate::DictationMode;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc::Sender,
};

const SHORTCUTS: [&str; 6] = [
    "",
    "CTRL + SHIFT + D",
    "CTRL + ALT + D",
    "SUPER + SHIFT + D",
    "SUPER + F9",
    "SUPER + F10",
];

#[derive(Clone)]
struct Draft {
    settings: Settings,
    models: Vec<String>,
}
impl Draft {
    fn new(settings: &Settings, models: &[String]) -> Self {
        Self {
            settings: settings.clone(),
            models: models.to_vec(),
        }
    }
    fn choose_model(&mut self, model: &str, mode: DictationMode) {
        if model.is_empty() || self.models.iter().any(|item| item == model) {
            match mode {
                DictationMode::Transcription => self.settings.model = model.into(),
                DictationMode::Formal => self.settings.model_formal = model.into(),
            }
        }
    }
    fn server_saved(&mut self, url: &str, api_key: Option<&str>) {
        self.settings.base_url = url.into();
        self.settings.api_key = api_key.map(str::to_owned);
        self.models.clear();
    }
}
fn selected(items: &[String], value: &str) -> u32 {
    items
        .iter()
        .position(|item| item == value)
        .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32)
}

struct Widgets {
    window: gtk::ApplicationWindow,
    model: gtk::DropDown,
    model_formal: gtk::DropDown,
    shortcut: gtk::DropDown,
    shortcut_formal: gtk::DropDown,
    formal: gtk::CheckButton,
    microphone: gtk::DropDown,
    monitor: gtk::DropDown,
    temperature: gtk::SpinButton,
    prompt: gtk::TextView,
    prompt_formal: gtk::TextView,
    url: gtk::Entry,
    api_key: gtk::PasswordEntry,
    use_environment: gtk::CheckButton,
    status: gtk::Label,
    data: RefCell<Draft>,
    updating: Cell<bool>,
    microphones: Vec<Microphone>,
    monitors: Vec<MonitorInfo>,
    tx: Sender<Event>,
}

thread_local! {
    static APPLICATION: std::cell::OnceCell<gtk::Application> = const { std::cell::OnceCell::new() };
}
fn application() -> Result<gtk::Application> {
    APPLICATION.with(|cell| {
        if let Some(app) = cell.get() {
            return Ok(app.clone());
        }
        let app = gtk::Application::builder()
            .application_id("dev.j0k3r.HyprDictate")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>)?;
        let _ = cell.set(app.clone());
        Ok(app)
    })
}
pub struct Preferences(Rc<Widgets>);
impl Preferences {
    pub fn new(
        settings: &Settings,
        models: &[String],
        microphones: &[Microphone],
        monitors: &[MonitorInfo],
        message: &str,
        tx: Sender<Event>,
    ) -> Result<Self> {
        gtk::init()?;
        let app = application()?;
        let window = gtk::ApplicationWindow::builder()
            .application(&app)
            .title("Hypr Dictate — Ajustes")
            .default_width(680)
            .default_height(700)
            .build();
        window.set_icon_name(Some("audio-input-microphone-symbolic"));
        let header = gtk::HeaderBar::new();
        let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        title.append(&gtk::Image::from_icon_name(
            "audio-input-microphone-symbolic",
        ));
        title.append(&gtk::Label::new(Some("Ajustes de Hypr Dictate")));
        header.set_title_widget(Some(&title));
        window.set_titlebar(Some(&header));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        let switcher = gtk::StackSwitcher::builder()
            .stack(&stack)
            .halign(gtk::Align::Center)
            .build();
        root.append(&switcher);
        root.append(&stack);

        let general = page();
        let model = dropdown();
        let model_formal = dropdown();
        let shortcut = gtk::DropDown::from_strings(&[
            "Sin atajo",
            SHORTCUTS[1],
            SHORTCUTS[2],
            SHORTCUTS[3],
            SHORTCUTS[4],
            SHORTCUTS[5],
        ]);
        let shortcut_formal = gtk::DropDown::from_strings(&[
            "Sin atajo",
            SHORTCUTS[1],
            SHORTCUTS[2],
            SHORTCUTS[3],
            SHORTCUTS[4],
            SHORTCUTS[5],
        ]);
        let formal = gtk::CheckButton::with_label("Redacción profesional al usar el dock");
        formal.set_tooltip_text(Some("Interpreta tu encargo hablado y escribe el texto solicitado para su canal, destinatario y tono, sin inventar datos ni leer otras aplicaciones."));
        let microphone = dropdown();
        let monitor = dropdown();
        row(
            &general,
            "applications-science-symbolic",
            "Modelo de dictado",
            "Elige la ruta completa cuenta/modelo del catálogo; debe aceptar audio.",
            &model,
        );
        row(
            &general,
            "applications-science-symbolic",
            "Modelo de redacción profesional",
            "Elige la ruta completa cuenta/modelo; no cambia el modelo de dictado y debe aceptar audio.",
            &model_formal,
        );
        row(
            &general,
            "input-keyboard-symbolic",
            "Atajo de dictado",
            "Mantén pulsado para hablar y suelta para transcribir.",
            &shortcut,
        );
        row(
            &general,
            "input-keyboard-symbolic",
            "Atajo de redacción profesional",
            "Mantén pulsado para redactar. Debe ser distinto al atajo de dictado.",
            &shortcut_formal,
        );
        general.append(&formal);
        row(
            &general,
            "audio-input-microphone-symbolic",
            "Micrófono",
            "Automático usa la entrada predeterminada de PipeWire.",
            &microphone,
        );
        row(
            &general,
            "video-display-symbolic",
            "Pantalla del dock",
            "Se aplica al guardar; no mueve la ventana de ajustes.",
            &monitor,
        );
        let general_scroll = gtk::ScrolledWindow::builder()
            .child(&general)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        stack.add_titled(&general_scroll, Some("general"), "General");

        let ai = page();
        let temperature = gtk::SpinButton::with_range(0., 1., 0.05);
        temperature.set_digits(2);
        row(
            &ai,
            "preferences-system-symbolic",
            "Temperatura",
            "Un valor bajo favorece respuestas más consistentes.",
            &temperature,
        );
        let prompt_label = gtk::Label::new(Some("Prompt del sistema"));
        prompt_label.set_xalign(0.);
        prompt_label.add_css_class("heading");
        ai.append(&prompt_label);
        let prompt = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .monospace(true)
            .left_margin(12)
            .right_margin(12)
            .top_margin(12)
            .bottom_margin(12)
            .accepts_tab(false)
            .vexpand(true)
            .build();
        prompt_label.set_mnemonic_widget(Some(&prompt));
        prompt.buffer().set_enable_undo(true);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&prompt)
            .vexpand(true)
            .min_content_height(240)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .has_frame(true)
            .build();
        ai.append(&scroll);
        let formal_label = gtk::Label::new(Some("Prompt de redacción profesional"));
        formal_label.set_xalign(0.);
        formal_label.add_css_class("heading");
        ai.append(&formal_label);
        let prompt_formal = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .monospace(true)
            .left_margin(12)
            .right_margin(12)
            .top_margin(12)
            .bottom_margin(12)
            .accepts_tab(false)
            .vexpand(true)
            .build();
        prompt_formal.buffer().set_enable_undo(true);
        formal_label.set_mnemonic_widget(Some(&prompt_formal));
        ai.append(
            &gtk::ScrolledWindow::builder()
                .child(&prompt_formal)
                .vexpand(true)
                .min_content_height(180)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .has_frame(true)
                .build(),
        );
        let restore = button(
            "edit-undo-symbolic",
            "Restaurar valores",
            "Restaura el prompt y la temperatura; pulsa Guardar cambios para aplicarlos.",
        );
        restore.set_halign(gtk::Align::Start);
        ai.append(&restore);
        let ai_scroll = gtk::ScrolledWindow::builder()
            .child(&ai)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        stack.add_titled(&ai_scroll, Some("ai"), "Prompts e IA");

        let server = page();
        let url = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text(settings::DEFAULT_BASE_URL)
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        row(
            &server,
            "network-server-symbolic",
            "URL base del servidor OpenAI-compatible",
            "URL base exacta del proveedor: solo se añaden /models y /chat/completions. Incluye /v1 únicamente si tu proveedor lo requiere.",
            &url,
        );
        let api_key = gtk::PasswordEntry::builder()
            .hexpand(true)
            .show_peek_icon(true)
            .build();
        row(
            &server,
            "dialog-password-symbolic",
            "API key",
            "Se guarda en texto plano en settings.json con permisos 0600. Vacía permite usar un servidor sin autenticación.",
            &api_key,
        );
        let use_environment =
            gtk::CheckButton::with_label("Usar CLIPROXYAPI_API_KEY (si está definida)");
        server.append(&use_environment);
        let weak_key = api_key.downgrade();
        use_environment.connect_toggled(move |field| {
            if let Some(key) = weak_key.upgrade() {
                key.set_sensitive(!field.is_active());
            }
        });
        let save_server = button(
            "network-server-symbolic",
            "Guardar servidor y API key",
            "Guarda solo la URL y la clave y recarga el catálogo, incluso si el servidor anterior no responde.",
        );
        save_server.set_halign(gtk::Align::Start);
        server.append(&save_server);
        let help = gtk::Label::new(Some(
            "Guardar servidor y API key no aplica los demás cambios pendientes.\nLa clave guardada queda en texto plano con permisos solo para tu usuario.\nDespués, elige un modelo compatible en General.",
        ));
        help.set_wrap(true);
        help.set_xalign(0.);
        help.add_css_class("dim-label");
        server.append(&help);
        stack.add_titled(&server, Some("server"), "Servidor y API key");

        let status = gtk::Label::builder()
            .wrap(true)
            .xalign(0.)
            .selectable(true)
            .build();
        status.add_css_class("dim-label");
        root.append(&status);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let close = button(
            "window-close-symbolic",
            "Cerrar",
            "Cierra y descarta los cambios que no hayas guardado.",
        );
        let save = button(
            "document-save-symbolic",
            "Guardar cambios",
            "Guarda modelo, atajo, micrófono, pantalla y parámetros de IA. La URL se guarda en Servidor.",
        );
        save.add_css_class("suggested-action");
        actions.append(&close);
        actions.append(&save);
        root.append(&actions);
        window.set_child(Some(&root));
        let keys = gtk::EventControllerKey::new();
        let weak_window = window.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(window) = weak_window.upgrade() {
                    window.close();
                }
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        window.add_controller(keys);

        let widgets = Rc::new(Widgets {
            window,
            model,
            model_formal,
            shortcut,
            shortcut_formal,
            formal,
            microphone,
            monitor,
            temperature,
            prompt,
            prompt_formal,
            url,
            api_key,
            use_environment,
            status,
            data: RefCell::new(Draft::new(settings, models)),
            updating: Cell::new(false),
            microphones: microphones.to_vec(),
            monitors: monitors.to_vec(),
            tx,
        });
        widgets.load();
        widgets.status.set_text(message);
        for (model_field, mode) in [
            (&widgets.model, DictationMode::Transcription),
            (&widgets.model_formal, DictationMode::Formal),
        ] {
            let weak = Rc::downgrade(&widgets);
            model_field.connect_selected_notify(move |field| {
                if let Some(w) = weak.upgrade()
                    && !w.updating.get()
                {
                    let models = w.data.borrow().models.clone();
                    let model = field
                        .selected()
                        .checked_sub(1)
                        .and_then(|i| models.get(i as usize))
                        .map_or("", String::as_str);
                    w.data.borrow_mut().choose_model(model, mode);
                }
            });
        }
        let weak = Rc::downgrade(&widgets);
        save.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let _ = w.tx.send(Event::Save(w.snapshot()));
            }
        });
        let weak = Rc::downgrade(&widgets);
        save_server.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let key = if w.use_environment.is_active() {
                    None
                } else {
                    Some(w.api_key.text().into())
                };
                let _ = w.tx.send(Event::SaveServer(w.url.text().into(), key));
            }
        });
        let weak = Rc::downgrade(&widgets);
        close.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.window.close();
            }
        });
        let weak = Rc::downgrade(&widgets);
        restore.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.prompt.buffer().set_text(settings::DEFAULT_PROMPT);
                w.prompt_formal
                    .buffer()
                    .set_text(settings::DEFAULT_PROMPT_FORMAL);
                w.temperature.set_value(settings::DEFAULT_TEMPERATURE);
                w.status.set_text(
                    "Valores restaurados en el borrador; pulsa Guardar cambios para aplicarlos.",
                );
            }
        });
        Ok(Self(widgets))
    }
    pub fn present(&self) {
        self.0.window.present();
    }
    pub fn close(&self) {
        self.0.window.close();
    }
    pub fn visible(&self) -> bool {
        self.0.window.is_visible()
    }
    pub fn set_message(&self, message: &str) {
        if self.0.status.text() != message {
            self.0.status.set_text(message);
        }
    }
    pub fn catalog_loaded(&self, models: &[String]) {
        self.0.data.borrow_mut().models = models.to_vec();
        self.0.rebuild_models();
    }
    pub fn server_saved(&self, base_url: &str, api_key: Option<&str>) {
        self.0.data.borrow_mut().server_saved(base_url, api_key);
        self.0.url.set_text(base_url);
        self.0.api_key.set_text(api_key.unwrap_or(""));
        self.0.use_environment.set_active(api_key.is_none());
        self.0.rebuild_models();
    }
    pub fn position_saved(&self, x: i32) {
        self.0.data.borrow_mut().settings.x = x;
    }
}
impl Widgets {
    fn load(&self) {
        let data = self.data.borrow();
        self.url.set_text(&data.settings.base_url);
        self.api_key
            .set_text(data.settings.api_key.as_deref().unwrap_or(""));
        self.use_environment
            .set_active(data.settings.api_key.is_none());
        self.api_key.set_sensitive(data.settings.api_key.is_some());
        self.formal.set_active(data.settings.formal);
        self.prompt_formal
            .buffer()
            .set_text(&data.settings.prompt_formal);
        self.shortcut_formal.set_selected(
            SHORTCUTS
                .iter()
                .position(|s| *s == data.settings.shortcut_formal)
                .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32),
        );
        self.temperature.set_value(data.settings.temperature);
        self.prompt.buffer().set_text(&data.settings.prompt);
        self.shortcut.set_selected(
            SHORTCUTS
                .iter()
                .position(|s| *s == data.settings.shortcut)
                .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32),
        );
        let mut microphones = vec![String::new()];
        microphones.extend(self.microphones.iter().map(|m| m.name.clone()));
        let mut labels = vec!["Automático".to_owned()];
        labels.extend(self.microphones.iter().map(|m| m.description.clone()));
        set_options(
            &self.microphone,
            &labels,
            selected(&microphones, &data.settings.microphone),
        );
        let mut monitors = vec![String::new()];
        monitors.extend(self.monitors.iter().map(|m| m.name.clone()));
        let mut labels = vec!["Automática (enfocada)".to_owned()];
        labels.extend(
            self.monitors
                .iter()
                .map(|m| format!("{} — {}", m.name, m.description)),
        );
        set_options(
            &self.monitor,
            &labels,
            selected(&monitors, &data.settings.monitor),
        );
        drop(data);
        self.rebuild_models();
    }
    fn rebuild_models(&self) {
        self.updating.set(true);
        let data = self.data.borrow();
        for (field, mode) in [
            (&self.model, DictationMode::Transcription),
            (&self.model_formal, DictationMode::Formal),
        ] {
            let models = &data.models;
            let index = models
                .iter()
                .position(|model| model == data.settings.model_for(mode))
                .map_or(0, |i| i as u32 + 1);
            let mut labels = vec!["Selecciona un modelo…".to_owned()];
            labels.extend(models.iter().cloned());
            set_options(field, &labels, index);
        }
        self.updating.set(false);
    }
    fn snapshot(&self) -> Settings {
        let mut settings = self.data.borrow().settings.clone();
        settings.shortcut = SHORTCUTS
            .get(self.shortcut.selected() as usize)
            .unwrap_or(&"")
            .to_string();
        settings.microphone = self
            .microphone
            .selected()
            .checked_sub(1)
            .and_then(|i| self.microphones.get(i as usize))
            .map_or(String::new(), |m| m.name.clone());
        settings.monitor = self
            .monitor
            .selected()
            .checked_sub(1)
            .and_then(|i| self.monitors.get(i as usize))
            .map_or(String::new(), |m| m.name.clone());
        settings.formal = self.formal.is_active();
        settings.shortcut_formal = SHORTCUTS
            .get(self.shortcut_formal.selected() as usize)
            .unwrap_or(&"")
            .to_string();
        let buffer = self.prompt_formal.buffer();
        settings.prompt_formal = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .into();
        settings.temperature = self.temperature.value();
        let buffer = self.prompt.buffer();
        settings.prompt = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .into();
        settings
    }
}
fn dropdown() -> gtk::DropDown {
    let field = gtk::DropDown::builder()
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<gtk::Expression>,
            "string",
        ))
        .enable_search(true)
        .hexpand(true)
        .build();
    // GTK 4.12+ can match a model name anywhere in its account/model route.
    if field.find_property("search-match-mode").is_some() {
        field.set_property("search-match-mode", gtk::StringFilterMatchMode::Substring);
    }
    field
}
fn set_options(field: &gtk::DropDown, items: &[String], selected: u32) {
    let labels: Vec<_> = items.iter().map(String::as_str).collect();
    field.set_model(Some(&gtk::StringList::new(&labels)));
    field.set_selected(selected);
}
fn page() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, 14)
}
fn row(parent: &gtk::Box, icon: &str, title: &str, hint: &str, field: &impl IsA<gtk::Widget>) {
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.append(&gtk::Image::from_icon_name(icon));
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.);
    label.set_mnemonic_widget(Some(field));
    label.add_css_class("heading");
    heading.append(&label);
    parent.append(&heading);
    field.set_tooltip_text(Some(hint));
    parent.append(field);
}
fn button(icon: &str, title: &str, hint: &str) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(&gtk::Label::new(Some(title)));
    let button = gtk::Button::builder()
        .child(&content)
        .tooltip_text(hint)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    button
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an isolated GTK display (Broadway), not the user's compositor"]
    fn native_widgets_preserve_drafts_signals_and_close_without_saving() {
        let (tx, rx) = std::sync::mpsc::channel();
        let saved = Settings::default();
        let models = vec!["plain".into(), "account/audio".into()];
        let preferences = Preferences::new(&saved, &models, &[], &[], "test", tx.clone()).unwrap();
        let w = &preferences.0;
        assert_eq!(w.model.selected(), 0);
        for field in [&w.model, &w.model_formal] {
            let expression = field.expression().expect("search needs a text expression");
            let filter = gtk::StringFilter::new(Some(expression));
            if field.find_property("search-match-mode").is_some() {
                let mode = field.property::<gtk::StringFilterMatchMode>("search-match-mode");
                assert_eq!(mode, gtk::StringFilterMatchMode::Substring);
                filter.set_match_mode(mode);
            }
            filter.set_ignore_case(true);
            filter.set_search(Some("AuDiO"));
            let filtered = gtk::FilterListModel::new(field.model(), Some(filter));
            assert_eq!(filtered.n_items(), 1);
            assert_eq!(
                filtered
                    .item(0)
                    .unwrap()
                    .downcast::<gtk::StringObject>()
                    .unwrap()
                    .string(),
                "account/audio"
            );
            assert_eq!(field.selected(), 0, "search must not select implicitly");
        }
        let labels = w
            .model
            .model()
            .unwrap()
            .downcast::<gtk::StringList>()
            .unwrap();
        assert_eq!(labels.n_items(), 3);
        assert_eq!(labels.string(1).unwrap(), "plain");
        assert_eq!(labels.string(2).unwrap(), "account/audio");
        w.model.set_selected(2);
        assert_eq!(w.model_formal.selected(), 0);
        w.model_formal.set_selected(1);
        assert_eq!(w.snapshot().model_formal, "plain");
        assert_eq!(w.snapshot().model, "account/audio");
        w.model_formal.set_selected(2);
        assert_eq!(w.snapshot().model_formal, "account/audio");
        assert_eq!(w.snapshot().model, "account/audio");
        w.model_formal.set_selected(0);
        assert!(w.snapshot().model_formal.is_empty());
        w.model_formal.set_selected(1);
        assert_eq!(w.snapshot().model_formal, "plain");
        assert!(w.api_key.shows_peek_icon());
        assert!(w.use_environment.is_active());
        assert!(!w.api_key.is_sensitive());
        w.use_environment.set_active(false);
        assert!(w.api_key.is_sensitive());
        w.api_key.set_text("draft-secret");
        w.formal.set_active(true);
        w.shortcut_formal.set_selected(5);
        w.prompt_formal.buffer().set_text("Redacta este mensaje.");
        w.prompt.buffer().set_text("áñ🙂\nsecond line");
        assert!(w.prompt.buffer().enables_undo());
        let buffer = w.prompt.buffer();
        buffer.insert_at_cursor("extra");
        assert!(buffer.can_undo());
        buffer.undo();
        w.url.set_text("https://new.example");
        let snapshot = w.snapshot();
        assert_eq!(snapshot.model, "account/audio");
        assert_eq!(snapshot.model_formal, "plain");
        assert_eq!(snapshot.prompt, "áñ🙂\nsecond line");
        assert_eq!(snapshot.base_url, settings::DEFAULT_BASE_URL);
        assert!(
            snapshot.api_key.is_none(),
            "general save must not commit the key draft"
        );
        assert!(snapshot.formal);
        assert_eq!(snapshot.shortcut_formal, "SUPER + F10");
        assert_eq!(snapshot.prompt_formal, "Redacta este mensaje.");
        fn click(widget: &gtk::Widget, title: &str) -> bool {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && let Some(content) = button.child()
                && let Some(last) = content.last_child()
                && last
                    .downcast_ref::<gtk::Label>()
                    .is_some_and(|label| label.text() == title)
            {
                button.emit_clicked();
                return true;
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if click(&widget, title) {
                    return true;
                }
                child = widget.next_sibling();
            }
            false
        }
        assert!(click(w.window.upcast_ref(), "Guardar cambios"));
        assert!(
            matches!(rx.try_recv().unwrap(), Event::Save(settings) if settings.prompt == snapshot.prompt && settings.base_url == settings::DEFAULT_BASE_URL && settings.model_formal == "plain" && settings.model == "account/audio")
        );
        assert!(click(w.window.upcast_ref(), "Guardar servidor y API key"));
        assert!(
            matches!(rx.try_recv().unwrap(), Event::SaveServer(url, Some(key)) if url == "https://new.example" && key == "draft-secret")
        );
        preferences.server_saved("https://new.example", Some("draft-secret"));
        assert_eq!(w.snapshot().api_key.as_deref(), Some("draft-secret"));
        assert_eq!(w.snapshot().model, "account/audio");
        assert_eq!(w.snapshot().model_formal, "plain");
        assert!(w.snapshot().formal);
        assert_eq!(w.snapshot().prompt_formal, "Redacta este mensaje.");
        assert_eq!(w.snapshot().prompt, snapshot.prompt);
        assert_eq!(w.snapshot().base_url, "https://new.example");
        preferences.catalog_loaded(&models);
        assert_eq!(w.model.selected(), 2);
        assert_eq!(w.model_formal.selected(), 1);
        assert_eq!(w.snapshot().model_formal, "plain");
        let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
        for icon in [
            "document-save-symbolic",
            "edit-undo-symbolic",
            "window-close-symbolic",
            "audio-input-microphone-symbolic",
            "network-server-symbolic",
            "applications-science-symbolic",
            "input-keyboard-symbolic",
            "video-display-symbolic",
            "preferences-system-symbolic",
        ] {
            assert!(theme.has_icon(icon), "missing icon: {icon}");
        }
        preferences.present();
        assert!(preferences.visible());
        preferences.close();
        assert!(!preferences.visible());
        assert!(rx.try_recv().is_err());
        let reopened = Preferences::new(&saved, &models, &[], &[], "test", tx).unwrap();
        assert_eq!(reopened.0.snapshot().prompt, saved.prompt);
        assert!(!reopened.0.snapshot().formal);
        assert!(reopened.0.snapshot().model_formal.is_empty());
        assert_eq!(reopened.0.model_formal.selected(), 0);
        assert_eq!(reopened.0.snapshot().prompt_formal, saved.prompt_formal);
        assert!(reopened.0.api_key.text().is_empty());
        assert!(reopened.0.use_environment.is_active());
        reopened.close();
    }

    #[test]
    fn complete_model_routes_can_be_selected_without_an_account_filter() {
        let saved = Settings::default();
        let mut draft = Draft::new(&saved, &["account/audio".into(), "other/redaction".into()]);
        draft.choose_model("account/audio", DictationMode::Transcription);
        draft.choose_model("other/redaction", DictationMode::Formal);
        assert_eq!(draft.settings.model, "account/audio");
        assert_eq!(draft.settings.model_formal, "other/redaction");
        draft.choose_model("other/redaction", DictationMode::Transcription);
        assert_eq!(draft.settings.model, "other/redaction");
        assert_eq!(draft.settings.model_formal, "other/redaction");
        draft.choose_model("", DictationMode::Formal);
        assert!(draft.settings.model_formal.is_empty());
        assert_eq!(draft.settings.model, "other/redaction");
        draft.choose_model("unknown/model", DictationMode::Transcription);
        assert_eq!(draft.settings.model, "other/redaction");
    }

    #[test]
    fn catalog_never_selects_a_model_implicitly_and_modes_are_independent() {
        let saved = Settings::default();
        let mut draft = Draft::new(&saved, &["plain".into(), "account/audio".into()]);
        assert!(draft.settings.model.is_empty());
        assert_eq!(
            selected(&draft.models, &draft.settings.model),
            gtk::INVALID_LIST_POSITION
        );
        assert!(draft.settings.model_formal.is_empty());
        draft.choose_model("account/audio", DictationMode::Transcription);
        assert_eq!(draft.settings.model, "account/audio");
        draft.choose_model("plain", DictationMode::Formal);
        assert_eq!(draft.settings.model_formal, "plain");
        draft.choose_model("", DictationMode::Transcription);
        assert!(draft.settings.model.is_empty());
        assert!(saved.model.is_empty());
    }
    #[test]
    fn saved_model_is_preserved_and_server_save_does_not_commit_other_drafts() {
        let saved = Settings {
            model: "account/audio".into(),
            model_formal: "other/redaction".into(),
            ..Settings::default()
        };
        let mut draft = Draft::new(&saved, &["account/audio".into(), "other/redaction".into()]);
        assert_eq!(selected(&draft.models, &draft.settings.model_formal), 1);
        assert_eq!(selected(&draft.models, &draft.settings.model), 0);
        draft.settings.prompt = "pending".into();
        draft.server_saved("https://new.example", None);
        assert_eq!(draft.settings.prompt, "pending");
        assert_eq!(draft.settings.model, "account/audio");
        assert_eq!(draft.settings.model_formal, "other/redaction");
        assert!(draft.models.is_empty());
        assert_ne!(saved.prompt, draft.settings.prompt);
        assert_eq!(saved.base_url, settings::DEFAULT_BASE_URL);
        assert_eq!(Draft::new(&saved, &[]).settings.prompt, saved.prompt);
    }
}
