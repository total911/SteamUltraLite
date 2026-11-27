#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod account;
mod catalog;
mod library;

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
    root: String,
    account: account::Account,
    installed: Vec<Game>,
    view: View,
    query: String,
    visible: Vec<usize>,
    problem: Option<String>,
    scan: Option<Receiver<std::io::Result<Vec<Game>>>>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let russian = sys_locale::get_locale().is_some_and(|locale| locale.starts_with("ru"));
        let root = library::default_root().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
        let mut app = Self {
            russian,
            root,
            account: account::Account::default(),
            installed: Vec::new(),
            view: View::Installed,
            query: String::new(),
            visible: Vec::new(),
            problem: None,
            scan: None,
        };
        if !app.root.is_empty() {
            app.scan(ctx);
        }
        app
    }

    fn scan(&mut self, ctx: &egui::Context) {
        let root = PathBuf::from(self.root.trim());
        let (sender, receiver) = channel();
        let repaint = ctx.clone();
        self.problem = None;
        self.scan = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(library::scan(&root));
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

    fn games(&self) -> &[Game] {
        self.view.games(&self.installed, self.account.games())
    }

    fn refresh_visible(&mut self) {
        self.visible = library::matches(self.games(), &self.query);
    }

    fn set_view(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.refresh_visible();
        }
    }

    fn folder_ui(&mut self, ui: &mut egui::Ui) {
        let russian = self.russian;
        ui.horizontal(|ui| {
            ui.label(if russian { "Папка Steam" } else { "Steam folder" });
            let root = ui.add_enabled(
                self.scan.is_none(),
                TextEdit::singleline(&mut self.root).desired_width(280.0),
            );
            let enabled = self.scan.is_none() && !self.root.trim().is_empty();
            let button = ui.add_enabled(enabled, egui::Button::new(if russian { "Читать" } else { "Scan" }));
            let enter = root.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if button.clicked() || enabled && enter {
                self.scan(ui.ctx());
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_scan();
        if self.account.poll(ui.ctx()) {
            self.set_view(if self.account.is_signed_in() { View::Library } else { View::Installed });
        }

        let russian = self.russian;
        let text = |ru: &'static str, en: &'static str| if russian { ru } else { en };
        CentralPanel::default().show(ui, |ui| {
            self.account.ui(ui, russian);
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
                self.folder_ui(ui);
            }

            let search = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text(text("Поиск по названию или AppID", "Search by title or AppID"))
                    .desired_width(f32::INFINITY),
            );
            if search.changed() {
                self.refresh_visible();
            }
            ui.separator();

            if self.view == View::Installed && self.scan.is_some() {
                ui.label(text("Чтение библиотеки…", "Reading library…"));
            } else if self.view == View::Installed && let Some(problem) = &self.problem {
                ui.label(text("Не удалось прочитать библиотеку", "Could not read the library"));
                ui.label(problem);
            } else if self.visible.is_empty() {
                ui.label(text("Игры не найдены", "No games found"));
            } else {
                let games = self.games();
                ScrollArea::vertical().id_salt(self.view == View::Library).show_rows(ui, 24.0, self.visible.len(), |ui, range| {
                    for index in range {
                        let label = egui::Label::new(&games[self.visible[index]].name).truncate();
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 24.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| ui.add(label),
                        );
                    }
                });
            }
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        TITLE,
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([680.0, 480.0]).with_min_inner_size([520.0, 320.0]),
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
