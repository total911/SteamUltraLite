use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

use client::state::{Phase, State};
use client::{Client, Command, Event, Link};
use egui::{Align, Layout, Margin, Ui, ViewportCommand};
use extensions::{Action, Env, Registry};
use i18n::{Lang, t};
use serde::{Deserialize, Serialize};
use ui::Theme;

use crate::{builtin, chrome, links, login, offline_ui};

pub const OFFSCREEN_POS: [f32; 2] = [-4000.0, 0.0];

pub fn offscreen() -> bool {
    std::env::args().any(|arg| arg == "--offscreen")
}

pub fn demo() -> bool {
    std::env::args().any(|arg| arg == "--demo")
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Prefs {
    #[serde(skip, default = "ui::themes::default_theme")]
    pub theme: Theme,
    language: Option<String>,
    pub wants_offline: bool,
    pub auto_offline: bool,
    auto_login: Option<String>,
    auto_updates: bool,
    cloud: bool,
    libraries: HashMap<model::SteamId, String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ui::themes::default_theme(),
            language: None,
            wants_offline: false,
            auto_offline: false,
            auto_login: None,
            auto_updates: true,
            cloud: true,
            libraries: HashMap::new(),
        }
    }
}

pub struct SteamApp {
    rt: tokio::runtime::Runtime,
    pub client: Client,
    pub state: State,
    pub prefs: Prefs,
    pub saved: Vec<client::store::SavedAccount>,
    pub login: login::LoginForm,
    pub demo: bool,
    local: local::LocalState,
    flags: builtin::settings::SharedFlags,
    registry: Registry,
    library_account: Option<model::SteamId>,
    events: mpsc::Receiver<Event>,
    links: mpsc::Receiver<String>,
    pending_links: Vec<String>,
    page: String,
    mode: Option<(bool, bool)>,
    message: Option<(String, String, f64)>,
    confirm_exit: bool,
    exiting: bool,
}

fn local_library(demo: bool) -> local::LocalState {
    if let Some(path) = crate::demo::fake_library_path() {
        return local::load_with(Some(path), &[]).unwrap_or_default();
    }
    if demo || std::env::args().any(|arg| arg == "--no-local") {
        local::LocalState::default()
    } else {
        client::store::load_local(None).unwrap_or_else(|error| {
            tracing::warn!("local Steam library: {error}");
            local::LocalState::default()
        })
    }
}

impl SteamApp {
    pub fn new(cc: &eframe::CreationContext<'_>, link: Option<String>) -> Self {
        let demo = demo();
        let prefs: Prefs = if demo { Prefs::default() } else { cc.storage.and_then(|s| eframe::get_value(s, "prefs")).unwrap_or_default() };
        let lang = std::env::args().find_map(|arg| arg.strip_prefix("--lang=").and_then(Lang::from_code));
        i18n::set_language(lang.or_else(|| prefs.language.as_deref().and_then(Lang::from_code)).unwrap_or_else(Lang::detect));
        ui::install_fonts(&cc.egui_ctx);
        prefs.theme.apply(&cc.egui_ctx);

        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
        let (tx, events) = mpsc::channel();
        let repaint = cc.egui_ctx.clone();
        let event_tx = tx.clone();
        let client = Client::spawn(rt.handle(), Arc::new(move |event| {
            let _ = event_tx.send(event);
            repaint.request_repaint();
        }));
        let local = local_library(demo);
        let saved = if demo { Vec::new() } else { client::store::load() };
        if demo {
            for event in crate::demo::events(&local) {
                let _ = tx.send(event);
            }
        } else if let Some(account) = prefs.auto_login.as_ref().filter(|account| saved.iter().any(|saved| &saved.account_name == *account)) {
            let account = account.clone();
            client.send(if prefs.wants_offline { Command::StartOffline { account } } else { Command::LoginSaved { account } });
        }
        let flags = Arc::new(Mutex::new(builtin::settings::AppFlags { auto_updates: prefs.auto_updates, cloud: prefs.cloud }));
        let registry = builtin::registry(rt.handle().clone(), cc.egui_ctx.clone(), flags.clone());
        let separate = demo || std::env::args().any(|arg| arg == "--separate");
        let links = if separate { mpsc::channel().1 } else { links::listen(cc.egui_ctx.clone()) };
        Self {
            rt,
            client,
            state: State::default(),
            prefs,
            saved,
            login: login::LoginForm::default(),
            demo,
            local,
            flags,
            registry,
            library_account: None,
            events,
            links,
            pending_links: link.into_iter().collect(),
            page: "library".into(),
            mode: None,
            message: None,
            confirm_exit: false,
            exiting: false,
        }
    }

    pub fn toast(&mut self, now: f64, title: impl Into<String>, body: impl Into<String>) {
        self.message = Some((title.into(), body.into(), now + 5.0));
    }

    fn save_library(&mut self) {
        if let Some(account) = self.library_account {
            if let Some((_, data)) = self.registry.save_all().into_iter().find(|(id, _)| id == "library") {
                self.prefs.libraries.insert(account, data);
            }
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let now = ctx.input(|i| i.time);
        while let Ok(event) = self.events.try_recv() {
            offline_ui::on_event(self, &event, now);
            self.state.apply(&event);
            match &event {
                Event::LoggedIn { steam_id, account } => {
                    if !self.demo {
                        self.prefs.auto_login = Some(account.clone());
                        if self.library_account != Some(*steam_id) {
                            self.save_library();
                            self.registry = builtin::registry(self.rt.handle().clone(), ctx.clone(), self.flags.clone());
                            if let Some(data) = self.prefs.libraries.get(steam_id) {
                                self.registry.load("library", data);
                            }
                            self.library_account = Some(*steam_id);
                        }
                    }
                    self.login = login::LoginForm::default();
                }
                Event::LoggedOut { reason } => {
                    if reason == "Logged out" {
                        self.prefs.auto_login = None;
                    }
                    self.saved = client::store::load();
                }
                Event::LoginFailed(_) => {
                    self.prefs.auto_login = None;
                    self.saved = client::store::load();
                    self.login.on_failed();
                }
                Event::AccountInfo { .. } if !self.demo => self.saved = client::store::load(),
                Event::Error(message) => self.toast(now, t!("Something went wrong"), i18n::tr_dyn(message)),
                Event::GameStopped { .. } => self.local = local_library(self.demo),
                Event::CloudReport { held_launch: Some(_), .. } => self.show_window(ctx),
                _ => {}
            }
            let env = Env { state: &self.state, local: &self.local, theme: &self.prefs.theme, zoom: 1.0, client: &self.client, actions };
            self.registry.broadcast(&event, env);
        }
    }

    fn show_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        if !offscreen() {
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    fn navigate(&mut self, page: String, arg: Option<String>) {
        if matches!(page.as_str(), "library" | "downloads" | "settings") {
            self.registry.navigate(&page, arg.as_deref());
            self.page = page;
        }
    }

    fn handle_links(&mut self, ctx: &egui::Context) {
        while let Ok(raw) = self.links.try_recv() {
            self.show_window(ctx);
            self.pending_links.push(raw);
        }
        if self.state.phase != Phase::LoggedIn || self.demo {
            return;
        }
        for raw in std::mem::take(&mut self.pending_links) {
            let action = match links::parse(&raw) {
                Some(links::Link::Run(id)) if id & 0x8000_0000 != 0 || self.local.games.iter().any(|g| g.app_id == id) => format!("play:{id}"),
                Some(links::Link::Run(id) | links::Link::Install(id)) => format!("install:{id}"),
                Some(links::Link::Uninstall(id)) => format!("uninstall:{id}"),
                Some(links::Link::Validate(id)) => format!("verify:{id}"),
                Some(links::Link::Page(page)) => {
                    self.navigate(page.into(), None);
                    continue;
                }
                _ => continue,
            };
            self.navigate("library".into(), Some(action));
        }
    }

    fn apply_actions(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Navigate { page, arg } => self.navigate(page, arg),
                Action::ReloadLocal => self.local = local_library(self.demo),
                Action::Toast(message) => self.toast(ctx.input(|i| i.time), message, ""),
                _ => {}
            }
        }
    }

    fn request_exit(&mut self, ctx: &egui::Context) {
        if !self.state.download_queue.is_empty() || self.state.downloads.values().any(active) {
            self.confirm_exit = true;
        } else {
            self.exiting = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn library_ui(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let theme = self.prefs.theme.clone();
        egui::Panel::top("compact-navigation").exact_size(40.0).resizable(false).show(ui, |ui| {
            chrome::drag_area(ui, ui.max_rect(), true);
            ui.horizontal_centered(|ui| {
                chrome::traffic_light_space(ui);
                for (page, label) in [("library", t!("Library")), ("downloads", t!("Downloads")), ("settings", t!("Settings"))] {
                    if ui.selectable_label(self.page == page, label).clicked() {
                        self.navigate(page.into(), None);
                    }
                }
                if let Some(status) = self.state.downloads.values().find(|d| active(d)) {
                    let label = if status.paused { t!("Paused") } else { t!("Downloading") };
                    ui.label(format!("{label} {:.0}%", status.fraction() * 100.0));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    chrome::window_buttons(ui, &theme, 36.0, true);
                    if ui.button(t!("Exit")).clicked() {
                        self.request_exit(ui.ctx());
                    }
                    if ui.add_enabled(!self.demo, egui::Button::new(t!("Sign out"))).clicked() {
                        self.client.send(Command::LogOut);
                    }
                    let status = match self.state.link {
                        Link::Online => t!("Online"),
                        Link::Offline => t!("Offline"),
                        Link::Reconnecting { .. } => t!("Reconnecting…"),
                    };
                    ui.menu_button(status, |ui| {
                        ui.add_enabled_ui(!self.demo, |ui| offline_ui::account_items(self, ui, &theme));
                    });
                });
            });
        });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme.bg).inner_margin(Margin::same(14))).show(ui, |ui| {
            match self.page.as_str() {
                "downloads" => self.downloads(ui),
                "settings" => self.settings(ui, actions),
                _ => {
                    let env = Env { state: &self.state, local: &self.local, theme: &theme, zoom: 1.0, client: &self.client, actions };
                    self.registry.show_page("library", ui, env);
                }
            }
            let env = Env { state: &self.state, local: &self.local, theme: &theme, zoom: 1.0, client: &self.client, actions };
            self.registry.with_ext::<builtin::library::Library, _>("library", env, |lib, cx| lib.background_dialogs(ui, cx));
        });
    }

    fn downloads(&self, ui: &mut Ui) {
        offline_ui::downloads_notice(self, ui, &self.prefs.theme);
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut statuses: Vec<_> = self.state.downloads.values().filter(|d| active(d)).collect();
            statuses.sort_unstable_by_key(|d| d.app_id);
            for d in statuses {
                ui.push_id(d.app_id, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        let app_id = d.app_id;
                        ui.label(self.state.app_name(app_id).unwrap_or_default());
                        ui.add(egui::ProgressBar::new(d.fraction()).desired_width(135.0).show_percentage());
                        let label = if d.paused { t!("Resume") } else { t!("Pause") };
                        if ui.add_enabled(!d.verifying() && (!d.paused || self.state.online()), egui::Button::new(label)).clicked() {
                            self.client.send(if d.paused { Command::ResumeDownload { app_id } } else { Command::PauseDownload { app_id } });
                        }
                        if ui.button(t!("Cancel")).clicked() {
                            self.client.send(if d.verifying() { Command::CancelVerify { app_id } } else { Command::CancelDownload { app_id } });
                        }
                    });
                });
            }
            for &app_id in &self.state.download_queue {
                ui.push_id(app_id, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(self.state.app_name(app_id).unwrap_or_default());
                        ui.label(t!("Queued"));
                        if ui.button(t!("Remove from download queue")).clicked() {
                            self.client.send(Command::CancelDownload { app_id });
                        }
                    });
                });
            }
            if self.state.download_queue.is_empty() && !self.state.downloads.values().any(active) {
                ui.label(t!("Nothing in the queue"));
            }
        });
    }

    fn settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let mut lang = i18n::language();
        egui::ComboBox::from_label(t!("Language")).selected_text(lang.native_name()).show_ui(ui, |ui| {
            for entry in Lang::ALL {
                ui.selectable_value(&mut lang, entry, entry.native_name());
            }
        });
        if lang != i18n::language() {
            i18n::set_language(lang);
            self.prefs.language = Some(lang.code().into());
        }
        let auto_changed = ui.checkbox(&mut self.prefs.auto_updates, t!("Automatic updates")).changed();
        let cloud_changed = ui.checkbox(&mut self.prefs.cloud, t!("Steam Cloud")).changed();
        if auto_changed || cloud_changed {
            let mut flags = self.flags.lock().unwrap();
            flags.auto_updates = self.prefs.auto_updates;
            flags.cloud = self.prefs.cloud;
        }
        if cloud_changed {
            let env = Env { state: &self.state, local: &self.local, theme: &self.prefs.theme, zoom: 1.0, client: &self.client, actions };
            self.registry.with_ext::<builtin::library::Library, _>("library", env, |lib, cx| lib.send_cloud_settings(cx));
        }
    }
}

fn active(d: &model::DownloadStatus) -> bool {
    !matches!(d.phase, model::DownloadPhase::Done | model::DownloadPhase::Failed)
}

impl eframe::App for SteamApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        self.drain_events(&ctx, &mut actions);
        self.handle_links(&ctx);
        let signed_in = self.state.phase == Phase::LoggedIn;
        let splash = login::wants_splash(self);
        if self.mode != Some((signed_in, splash)) {
            self.mode = Some((signed_in, splash));
            let size = if signed_in { [1060.0, 700.0] } else if splash { [380.0, 250.0] } else { [720.0, 470.0] };
            ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(if signed_in { [850.0, 500.0].into() } else { size.into() }));
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size.into()));
            ctx.send_viewport_cmd(ViewportCommand::Resizable(signed_in));
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting && (!self.state.download_queue.is_empty() || self.state.downloads.values().any(active)) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm_exit = true;
        }
        if signed_in {
            self.library_ui(ui, &mut actions);
            chrome::resize_handles(&ctx);
        } else {
            login::show(self, ui);
        }
        self.apply_actions(&ctx, actions);
        if let Some((title, body, until)) = &self.message {
            let now = ctx.input(|i| i.time);
            if now >= *until {
                self.message = None;
            } else {
                egui::Area::new(egui::Id::unique("library-status")).anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -16.0]).show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(380.0);
                        ui.label(title);
                        if !body.is_empty() {
                            ui.label(body);
                        }
                    });
                });
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(until - now));
            }
        }
        if self.confirm_exit {
            let response = egui::Modal::new(egui::Id::unique("exit-downloads")).show(&ctx, |ui| {
                ui.label(t!("A download is in progress"));
                ui.horizontal(|ui| {
                    if ui.button(t!("Exit anyway")).clicked() {
                        self.exiting = true;
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                    if ui.button(t!("Cancel")).clicked() {
                        self.confirm_exit = false;
                    }
                });
            });
            if response.should_close() {
                self.confirm_exit = false;
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.demo {
            self.save_library();
            eframe::set_value(storage, "prefs", &self.prefs);
        }
    }
}
