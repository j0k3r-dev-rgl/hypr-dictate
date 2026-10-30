use crate::{
    Event,
    settings::{self, Microphone, MonitorInfo, Settings},
};
use anyhow::Result;
use gtk4::{self as gtk, prelude::*};
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
    account: String,
}
impl Draft {
    fn new(settings: &Settings, models: &[String]) -> Self {
        Self {
            settings: settings.clone(),
            models: models.to_vec(),
            account: account_of(&settings.model).into(),
        }
    }
    fn accounts(&self) -> Vec<String> {
        let mut accounts = vec![String::new()];
        for model in &self.models {
            let account = account_of(model).to_owned();
            if !accounts.contains(&account) {
                accounts.push(account);
            }
        }
        if !accounts.contains(&self.account) {
            accounts.push(self.account.clone());
        }
        accounts
    }
    fn available(&self) -> Vec<String> {
        self.models
            .iter()
            .filter(|model| account_of(model) == self.account)
            .cloned()
            .collect()
    }
    fn choose_account(&mut self, account: String) {
        if self.account != account {
            self.account = account;
            self.settings.model.clear();
        }
    }
    fn choose_model(&mut self, model: &str) {
        if self.available().iter().any(|item| item == model) {
            self.settings.model = model.into();
        }
    }
    fn server_saved(&mut self, url: &str) {
        self.settings.base_url = url.into();
        self.models.clear();
    }
}
fn account_of(model: &str) -> &str {
    model.split_once('/').map_or("", |(account, _)| account)
}
fn selected(items: &[String], value: &str) -> u32 {
    items
        .iter()
        .position(|item| item == value)
        .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32)
}

struct Widgets {
    window: gtk::ApplicationWindow,
    account: gtk::DropDown,
    model: gtk::DropDown,
    shortcut: gtk::DropDown,
    microphone: gtk::DropDown,
    monitor: gtk::DropDown,
    temperature: gtk::SpinButton,
    prompt: gtk::TextView,
    url: gtk::Entry,
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
        let account = dropdown();
        let model = dropdown();
        let shortcut = gtk::DropDown::from_strings(&[
            "Sin atajo",
            SHORTCUTS[1],
            SHORTCUTS[2],
            SHORTCUTS[3],
            SHORTCUTS[4],
            SHORTCUTS[5],
        ]);
        let microphone = dropdown();
        let monitor = dropdown();
        row(
            &general,
            "avatar-default-symbolic",
            "Cuenta / ruta",
            "Automática muestra los modelos sin prefijo.",
            &account,
        );
        row(
            &general,
            "applications-science-symbolic",
            "Modelo",
            "Selecciona explícitamente un modelo que acepte audio.",
            &model,
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
        let restore = button(
            "edit-undo-symbolic",
            "Restaurar valores",
            "Restaura el prompt y la temperatura; pulsa Guardar cambios para aplicarlos.",
        );
        restore.set_halign(gtk::Align::Start);
        ai.append(&restore);
        stack.add_titled(&ai, Some("ai"), "Prompt e IA");

        let server = page();
        let url = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text(settings::DEFAULT_BASE_URL)
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        row(
            &server,
            "network-server-symbolic",
            "URL base de CLIProxyAPI",
            "HTTP o HTTPS, sin /v1, credenciales, parámetros ni fragmentos.",
            &url,
        );
        let save_server = button(
            "network-server-symbolic",
            "Guardar servidor",
            "Guarda solo la URL y recarga el catálogo, incluso si el servidor anterior no responde.",
        );
        save_server.set_halign(gtk::Align::Start);
        server.append(&save_server);
        let help = gtk::Label::new(Some(
            "Guardar servidor no aplica los demás cambios pendientes.\nDespués, elige un modelo compatible en General.\nLa clave se obtiene de CLIPROXYAPI_API_KEY; no se guarda aquí.",
        ));
        help.set_wrap(true);
        help.set_xalign(0.);
        help.add_css_class("dim-label");
        server.append(&help);
        stack.add_titled(&server, Some("server"), "Servidor");

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
            account,
            model,
            shortcut,
            microphone,
            monitor,
            temperature,
            prompt,
            url,
            status,
            data: RefCell::new(Draft::new(settings, models)),
            updating: Cell::new(false),
            microphones: microphones.to_vec(),
            monitors: monitors.to_vec(),
            tx,
        });
        widgets.load();
        widgets.status.set_text(message);
        let weak = Rc::downgrade(&widgets);
        widgets.account.connect_selected_notify(move |field| {
            if let Some(w) = weak.upgrade()
                && !w.updating.get()
            {
                let accounts = w.data.borrow().accounts();
                if let Some(account) = accounts.get(field.selected() as usize) {
                    w.data.borrow_mut().choose_account(account.clone());
                    w.rebuild_models();
                }
            }
        });
        let weak = Rc::downgrade(&widgets);
        widgets.model.connect_selected_notify(move |field| {
            if let Some(w) = weak.upgrade()
                && !w.updating.get()
            {
                let models = w.data.borrow().available();
                if let Some(model) = field
                    .selected()
                    .checked_sub(1)
                    .and_then(|i| models.get(i as usize))
                {
                    w.data.borrow_mut().choose_model(model);
                } else {
                    w.data.borrow_mut().settings.model.clear();
                }
            }
        });
        let weak = Rc::downgrade(&widgets);
        save.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let _ = w.tx.send(Event::Save(w.snapshot()));
            }
        });
        let weak = Rc::downgrade(&widgets);
        save_server.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                let _ = w.tx.send(Event::SaveServer(w.url.text().into()));
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
        self.0.rebuild_accounts();
    }
    pub fn server_saved(&self, base_url: &str) {
        self.0.data.borrow_mut().server_saved(base_url);
        self.0.url.set_text(base_url);
        self.0.rebuild_accounts();
    }
    pub fn position_saved(&self, x: i32) {
        self.0.data.borrow_mut().settings.x = x;
    }
}
impl Widgets {
    fn load(&self) {
        let data = self.data.borrow();
        self.url.set_text(&data.settings.base_url);
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
        self.rebuild_accounts();
    }
    fn rebuild_accounts(&self) {
        self.updating.set(true);
        let data = self.data.borrow();
        let accounts = data.accounts();
        let labels: Vec<_> = accounts
            .iter()
            .map(|a| {
                if a.is_empty() {
                    "Automática".into()
                } else {
                    a.clone()
                }
            })
            .collect();
        set_options(&self.account, &labels, selected(&accounts, &data.account));
        drop(data);
        self.updating.set(false);
        self.rebuild_models();
    }
    fn rebuild_models(&self) {
        self.updating.set(true);
        let data = self.data.borrow();
        let models = data.available();
        let index = models
            .iter()
            .position(|model| model == &data.settings.model)
            .map_or(0, |i| i as u32 + 1);
        let mut labels = vec!["Selecciona un modelo…".to_owned()];
        labels.extend(models);
        set_options(&self.model, &labels, index);
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
        settings.temperature = self.temperature.value();
        let buffer = self.prompt.buffer();
        settings.prompt = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .into();
        settings
    }
}
fn dropdown() -> gtk::DropDown {
    gtk::DropDown::builder()
        .enable_search(true)
        .hexpand(true)
        .build()
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
        w.account.set_selected(1);
        assert_eq!(w.model.selected(), 0);
        w.model.set_selected(1);
        w.prompt.buffer().set_text("áñ🙂\nsecond line");
        assert!(w.prompt.buffer().enables_undo());
        let buffer = w.prompt.buffer();
        buffer.insert_at_cursor("extra");
        assert!(buffer.can_undo());
        buffer.undo();
        w.url.set_text("https://new.example");
        let snapshot = w.snapshot();
        assert_eq!(snapshot.model, "account/audio");
        assert_eq!(snapshot.prompt, "áñ🙂\nsecond line");
        assert_eq!(snapshot.base_url, settings::DEFAULT_BASE_URL);
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
            matches!(rx.try_recv().unwrap(), Event::Save(settings) if settings.prompt == snapshot.prompt && settings.base_url == settings::DEFAULT_BASE_URL)
        );
        assert!(click(w.window.upcast_ref(), "Guardar servidor"));
        assert!(
            matches!(rx.try_recv().unwrap(), Event::SaveServer(url) if url == "https://new.example")
        );
        preferences.server_saved("https://new.example");
        assert_eq!(w.snapshot().prompt, snapshot.prompt);
        assert_eq!(w.snapshot().base_url, "https://new.example");
        preferences.catalog_loaded(&models);
        assert_eq!(w.model.selected(), 1);
        let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
        for icon in [
            "document-save-symbolic",
            "edit-undo-symbolic",
            "window-close-symbolic",
            "audio-input-microphone-symbolic",
            "network-server-symbolic",
            "avatar-default-symbolic",
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
        reopened.close();
    }

    #[test]
    fn catalog_never_selects_a_model_implicitly_and_account_changes_clear_selection() {
        let saved = Settings::default();
        let mut draft = Draft::new(&saved, &["plain".into(), "account/audio".into()]);
        assert!(draft.settings.model.is_empty());
        assert_eq!(
            selected(&draft.available(), &draft.settings.model),
            gtk::INVALID_LIST_POSITION
        );
        draft.choose_account("account".into());
        assert!(draft.settings.model.is_empty());
        draft.choose_model("account/audio");
        assert_eq!(draft.settings.model, "account/audio");
        draft.choose_account(String::new());
        assert!(draft.settings.model.is_empty());
        assert!(saved.model.is_empty());
    }
    #[test]
    fn saved_model_is_preserved_and_server_save_does_not_commit_other_drafts() {
        let saved = Settings {
            model: "account/audio".into(),
            ..Settings::default()
        };
        let mut draft = Draft::new(&saved, &["account/audio".into()]);
        assert_eq!(selected(&draft.available(), &draft.settings.model), 0);
        draft.settings.prompt = "pending".into();
        draft.server_saved("https://new.example");
        assert_eq!(draft.settings.prompt, "pending");
        assert_eq!(draft.settings.model, "account/audio");
        assert!(draft.models.is_empty());
        assert_ne!(saved.prompt, draft.settings.prompt);
        assert_eq!(saved.base_url, settings::DEFAULT_BASE_URL);
        assert_eq!(Draft::new(&saved, &[]).settings.prompt, saved.prompt);
    }
}
