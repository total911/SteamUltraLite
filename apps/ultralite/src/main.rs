#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod catalog;
mod library;
mod install;
mod launch;
// ponytail: cloud lives in this package to exclude the old client; extract a shared crate if both copies need maintenance.
mod cloud;
mod store;
#[path = "../../../crates/client/src/download.rs"]
mod download;
mod pics {
    pub use crate::catalog::{parse_depots, raw_app_info};
}

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{self, CentralPanel, ScrollArea, TextEdit};

use library::Game;

const TITLE: &str = "SteamUltraLite";

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Library,
    Installed,
}

impl View {
    fn games<'a>(self, installed: &'a [Game], owned: &'a [Game]) -> &'a [Game] {
        match self {
            Self::Library => owned,
            Self::Installed => installed,
        }
    }
}

struct App {
    russian: bool,
    roots: Vec<PathBuf>,
    account: account::Account,
    installed: Vec<Game>,
    view: View,
    selected: Option<u32>,
    install_target: Option<PathBuf>,
    query: String,
    visible: Vec<usize>,
    problem: Option<String>,
    scan: Option<Receiver<std::io::Result<Vec<Game>>>>,
    pending_remove: Option<u32>,
    pending_cloud_sync: Option<u32>,
    remove_task: Option<Receiver<Result<(), String>>>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let russian = sys_locale::get_locale().is_some_and(|locale| locale.starts_with("ru"));
        let roots = load_roots();
        let mut app = Self {
            russian,
            roots,
            account: account::Account::default(),
            installed: Vec::new(),
            view: View::Installed,
            selected: None,
            install_target: None,
            query: String::new(),
            visible: Vec::new(),
            problem: None,
            scan: None,
            pending_remove: None,
            pending_cloud_sync: None,
            remove_task: None,
        };
        if !app.roots.is_empty() {
            app.scan(ctx);
        }
        app
    }

    fn scan(&mut self, ctx: &egui::Context) {
        let roots = self.roots.clone();
        let (sender, receiver) = channel();
        let repaint = ctx.clone();
        self.problem = None;
        self.scan = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(library::scan_roots(&roots));
            repaint.request_repaint();
        });
    }

    fn receive_scan(&mut self) {
        let Some(scan) = &self.scan else { return };
        match scan.try_recv() {
            Ok(Ok(games)) => self.installed = games,
            Ok(Err(error)) => {
                self.installed.clear();
                self.problem = Some(error.to_string());
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.installed.clear();
                self.problem = Some(if self.russian { "Чтение прервано" } else { "Scan interrupted" }.into());
            }
        }
        self.scan = None;
        if self.view == View::Installed {
            self.refresh_visible();
        }
    }

    fn receive_remove(&mut self, ctx: &egui::Context) {
        let Some(task) = &self.remove_task else { return };
        match task.try_recv() {
            Ok(Ok(())) => {
                self.remove_task = None;
                self.selected = None;
                self.scan(ctx);
            }
            Ok(Err(error)) => {
                self.remove_task = None;
                self.problem = Some(error);
            }
            Err(TryRecvError::Disconnected) => {
                self.remove_task = None;
                self.problem = Some("Removal interrupted".into());
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn games(&self) -> &[Game] {
        self.view.games(&self.installed, self.account.games())
    }

    fn refresh_visible(&mut self) {
        self.visible = library::matches(self.games(), &self.query);
    }

    fn set_view(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.selected = None;
            self.pending_remove = None;
            self.pending_cloud_sync = None;
            self.refresh_visible();
        }
    }

    fn selected_game(&self) -> Option<Game> {
        let mut game = self.games().iter().find(|game| Some(game.id) == self.selected)?.clone();
        if self.view == View::Installed {
            if let Some(owned) = self.account.games().iter().find(|owned| owned.id == game.id) {
                game.developer = owned.developer.clone();
                game.publisher = owned.publisher.clone();
                game.achievements = owned.achievements;
                game.cards = owned.cards;
                game.playtime_minutes = owned.playtime_minutes;
                game.achievement_progress = owned.achievement_progress;
                game.content = owned.content.clone();
            }
        }
        Some(game)
    }

    fn add_folder(&mut self, ctx: &egui::Context) {
        let Some(mut path) = rfd::FileDialog::new().pick_folder() else { return };
        if path.file_name().is_some_and(|name| name.eq_ignore_ascii_case("steamapps")) {
            path.pop();
        }
        if !library::valid_root(&path) {
            self.problem = Some(if self.russian {
                "Выбери папку библиотеки Steam, содержащую steamapps".into()
            } else {
                "Choose a Steam library folder containing steamapps".into()
            });
            return;
        }
        if !self.roots.iter().any(|root| root.to_string_lossy().eq_ignore_ascii_case(&path.to_string_lossy())) {
            self.roots.push(path);
            save_roots(&self.roots);
            self.scan(ctx);
        }
    }

    fn folders_ui(&mut self, ui: &mut egui::Ui) {
        let russian = self.russian;
        ui.horizontal(|ui| {
            ui.label(if russian { "Папки Steam" } else { "Steam folders" });
            if ui.add_enabled(self.scan.is_none(), egui::Button::new(if russian { "Обзор…" } else { "Browse…" })).clicked() {
                self.add_folder(ui.ctx());
            }
        });
        let mut remove = None;
        for (index, root) in self.roots.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.add_enabled(self.scan.is_none(), egui::Button::new("×"))
                    .on_hover_text(if russian { "Убрать из списка; файлы останутся" } else { "Remove from list; files stay" })
                    .clicked()
                {
                    remove = Some(index);
                }
                ui.label(root.display().to_string());
            });
        }
        if let Some(index) = remove {
            self.roots.remove(index);
            save_roots(&self.roots);
            self.scan(ui.ctx());
        }
    }

    fn details_ui(&mut self, ui: &mut egui::Ui, game: &Game) {
        let russian = self.russian;
        let text = |ru: &'static str, en: &'static str| if russian { ru } else { en };
        let unknown = text("Нет данных", "Unknown");
        ui.heading(&game.name);
        ui.separator();
        ui.label(format!("{}: {}", text("Разработчик", "Developer"), game.developer.as_deref().unwrap_or(unknown)));
        ui.label(format!("{}: {}", text("Издатель", "Publisher"), game.publisher.as_deref().unwrap_or(unknown)));
        ui.label(format!("Steam AppID: {}", game.id));
        let availability = |value: Option<bool>| match value {
            Some(true) => text("Есть", "Available"),
            Some(false) => text("Нет", "None"),
            None => unknown,
        };
        ui.label(format!("{}: {}", text("Достижения", "Achievements"), availability(game.achievements)));
        ui.label(format!("{}: {}", text("Карточки Steam", "Steam Trading Cards"), availability(game.cards)));
        if self.view == View::Installed {
            if let Some(minutes) = game.playtime_minutes {
                ui.label(if russian { format!("Время в игре: {} ч {} мин", minutes / 60, minutes % 60) } else { format!("Playtime: {} h {} min", minutes / 60, minutes % 60) });
            } else {
                ui.label(format!("{}: {}", text("Время в игре", "Playtime"), unknown));
            }
            if let Some((unlocked, total)) = game.achievement_progress {
                ui.label(format!("{}: {unlocked}/{total}", text("Получено достижений", "Unlocked achievements")));
            }
            if self.account.is_signed_in() && game.content.as_ref().and_then(|app| app.cloud.as_ref()).is_some() {
                ui.separator();
                let cloud_ready = !self.account.cloud_running() && !self.account.installing();
                if ui.add_enabled(cloud_ready, egui::Button::new(text("Проверить Steam Cloud", "Check Steam Cloud"))).clicked() {
                    self.account.cloud(game, cloud::Mode::Check);
                    self.pending_cloud_sync = None;
                }
                if self.account.cloud_running() {
                    ui.label(text("Работа с облаком…", "Working with cloud…"));
                }
                if let Some(report) = self.account.cloud_report(game.id).cloned() {
                    ui.label(format!("{}: {}", text("Файлов в облаке", "Cloud files"), report.cloud_files));
                    ui.label(format!("{}: {}", text("Нужна загрузка на устройство", "Need download to device"), report.pending_download));
                    ui.label(format!("{}: {}", text("Нужна отправка в облако", "Need upload to cloud"), report.pending_upload));
                    ui.label(format!("{}: {}", text("Конфликтов", "Conflicts"), report.conflicts.len()));
                    ui.label(format!("{}: {} / {}", text("Выполнено: скачано / отправлено", "Completed: downloaded / uploaded"), report.downloaded, report.uploaded));
                    for (_, error) in &report.errors {
                        ui.label(error);
                    }
                    // ponytail: sync stays manual until launch lifecycle tracking can safely run it before and after play.
                    if cloud_ready && report.errors.is_empty() && report.conflicts.is_empty() {
                        if self.pending_cloud_sync == Some(game.id) {
                            ui.label(format!("{}: {} / {}", text("Подтверди скачивание / отправку", "Confirm download / upload"), report.pending_download, report.pending_upload));
                            if ui.button(text("Подтвердить синхронизацию", "Confirm sync")).clicked() {
                                self.account.cloud(game, cloud::Mode::Sync);
                                self.pending_cloud_sync = None;
                            }
                            if ui.button(text("Отмена", "Cancel")).clicked() {
                                self.pending_cloud_sync = None;
                            }
                        } else if ui.button(text("Синхронизировать", "Sync")).clicked() {
                            self.pending_cloud_sync = Some(game.id);
                        }
                    }
                }
            }
            if let Some(path) = &game.install_dir {
                if ui.button(text("Открыть папку", "Open folder")).clicked() {
                    if open::that_detached(path).is_err() {
                        self.problem = Some(text("Не удалось открыть папку", "Could not open the folder").into());
                    }
                }
            }
            let can_launch = game.install_dir.as_ref().is_some_and(|path| path.is_dir());
            let launch_label = if launch::uses_steam(game.id) {
                text("Запустить через Steam", "Launch through Steam")
            } else {
                text("Запустить", "Launch")
            };
            if ui.add_enabled(can_launch, egui::Button::new(launch_label)).clicked() {
                if let Err(error) = launch::start(game) {
                    self.problem = Some(error);
                }
            }
            let can_remove = game.install_dir.is_some() && game.library_root.is_some()
                && self.remove_task.is_none() && !self.account.installing();
            if self.pending_remove == Some(game.id) {
                ui.label(text(
                    "Удалятся файлы игры, включая сохранения внутри её папки.",
                    "Game files, including saves inside its folder, will be deleted.",
                ));
                if ui.button(text("Подтвердить удаление", "Confirm removal")).clicked() {
                    let (sender, receiver) = channel();
                    let game = game.clone();
                    let repaint = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = sender.send(install::remove(&game));
                        repaint.request_repaint();
                    });
                    self.remove_task = Some(receiver);
                    self.pending_remove = None;
                }
                if ui.button(text("Отмена", "Cancel")).clicked() {
                    self.pending_remove = None;
                }
            } else if ui.add_enabled(can_remove, egui::Button::new(text("Удалить с устройства", "Remove from device"))).clicked() {
                self.pending_remove = Some(game.id);
            }
        } else {
            ui.separator();
            ui.label(text("Куда установить", "Install location"));
            egui::ComboBox::from_id_salt("install_library")
                .selected_text(self.install_target.as_ref().map_or_else(
                    || text("Выбери папку", "Choose a folder").to_owned(),
                    |path| path.display().to_string(),
                ))
                .show_ui(ui, |ui| {
                    for root in &self.roots {
                        ui.selectable_value(&mut self.install_target, Some(root.clone()), root.display().to_string());
                    }
                });
            if ui.button(text("Добавить папку через обзор…", "Add a folder…")).clicked() {
                self.add_folder(ui.ctx());
            }
            let already_installed = self.installed.iter().any(|installed| installed.id == game.id);
            if already_installed {
                ui.label(text("Уже установлена в другой библиотеке", "Already installed in another library"));
            }
            let ready = self.account.is_signed_in() && !self.account.installing()
                && game.content.as_ref().is_some_and(|app| !app.depots.is_empty())
                && self.install_target.as_ref().is_some_and(|path| library::valid_root(path))
                && !already_installed;
            if ui.add_enabled(ready, egui::Button::new(text("Установить", "Install"))).clicked() {
                if let Some(root) = &self.install_target {
                    self.account.install(game, root.clone(), russian);
                }
            }
        }
        if ui.button(text("Открыть в браузере", "Open in browser")).clicked() {
            let url = format!("https://store.steampowered.com/app/{}/", game.id);
            if open::that_detached(url).is_err() {
                self.problem = Some(text("Не удалось открыть браузер", "Could not open the browser").into());
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_scan();
        self.receive_remove(ui.ctx());
        if self.account.poll(ui.ctx()) {
            self.set_view(if self.account.is_signed_in() { View::Library } else { View::Installed });
        }
        if self.account.take_installed().is_some() {
            self.scan(ui.ctx());
            self.set_view(View::Installed);
        }

        let russian = self.russian;
        let text = |ru: &'static str, en: &'static str| if russian { ru } else { en };
        CentralPanel::default().show(ui, |ui| {
            self.account.ui(ui, russian);
            self.account.install_ui(ui, russian);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(self.account.is_signed_in(), |ui| {
                    if ui.selectable_label(self.view == View::Library, text("Библиотека Steam", "Steam library")).clicked() {
                        self.set_view(View::Library);
                    }
                });
                if ui.selectable_label(self.view == View::Installed, text("Установленные", "Installed")).clicked() {
                    self.set_view(View::Installed);
                }
            });
            if self.view == View::Installed {
                self.folders_ui(ui);
            }
            if let Some(problem) = &self.problem {
                ui.label(problem);
            }
            ui.separator();
            ui.columns(2, |columns| {
                let left = &mut columns[0];
                let search = left.add(
                    TextEdit::singleline(&mut self.query)
                        .hint_text(text("Поиск по названию или AppID", "Search by title or AppID"))
                        .desired_width(f32::INFINITY),
                );
                if search.changed() {
                    self.refresh_visible();
                }
                left.separator();
                if self.view == View::Installed && self.scan.is_some() {
                    left.label(text("Чтение библиотеки…", "Reading library…"));
                } else if self.visible.is_empty() {
                    left.label(text("Игры не найдены", "No games found"));
                } else {
                    let mut picked = None;
                    let games = self.games();
                    ScrollArea::vertical().id_salt(self.view == View::Library).show_rows(left, 26.0, self.visible.len(), |ui, range| {
                        for index in range {
                            let game = &games[self.visible[index]];
                            if ui.selectable_label(self.selected == Some(game.id), &game.name).clicked() {
                                picked = Some(game.id);
                            }
                        }
                    });
                    if let Some(id) = picked {
                        self.selected = Some(id);
                    }
                }
                columns[1].separator();
                if let Some(game) = self.selected_game() {
                    self.details_ui(&mut columns[1], &game);
                } else {
                    columns[1].label(text("Выбери игру слева", "Select a game on the left"));
                }
            });
        });
    }
}

fn roots_file() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("SteamUltraLite").join("libraries.json"))
}

fn load_roots() -> Vec<PathBuf> {
    if let Some(path) = roots_file() {
        if let Ok(bytes) = std::fs::read(&path) {
            if bytes.len() <= 16 * 1024 {
                if let Ok(saved) = serde_json::from_slice::<Vec<PathBuf>>(&bytes) {
                    return saved.into_iter().filter(|path| library::valid_root(path)).collect();
                }
            }
        }
    }
    library::default_root().into_iter().collect()
}

fn save_roots(roots: &[PathBuf]) {
    let Some(path) = roots_file() else { return };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_ok() {
            if let Ok(bytes) = serde_json::to_vec(roots) {
                let _ = std::fs::write(path, bytes);
            }
        }
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        TITLE,
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([920.0, 580.0]).with_min_inner_size([720.0, 420.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_only_the_selected_library() {
        let installed = [Game::new(15, "Local game")];
        let owned = [Game::new(19, "Account game"), Game::new(15, "Steam name")];
        assert_eq!(library::matches(View::Library.games(&installed, &owned), "account"), [0]);
        assert!(library::matches(View::Installed.games(&installed, &owned), "19").is_empty());
        assert_eq!(library::matches(View::Installed.games(&installed, &owned), "local"), [0]);
        assert!(library::matches(View::Library.games(&installed, &owned), "local").is_empty());
    }
}
