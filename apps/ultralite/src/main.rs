#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod library;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{self, CentralPanel, ScrollArea, TextEdit};

const TITLE: &str = "SteamUltraLite";

struct App {
    russian: bool,
    root: String,
    games: Vec<library::Game>,
    query: String,
    visible: Vec<usize>,
    problem: Option<String>,
    scan: Option<Receiver<std::io::Result<Vec<library::Game>>>>,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let russian = sys_locale::get_locale().is_some_and(|locale| locale.starts_with("ru"));
        let root = library::default_root().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
        let mut app = Self {
            russian,
            root,
            games: Vec::new(),
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
            Ok(Ok(games)) => self.games = games,
            Ok(Err(error)) => {
                self.games.clear();
                self.problem = Some(error.to_string());
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.games.clear();
                self.problem = Some(if self.russian { "Чтение прервано" } else { "Scan interrupted" }.into());
            }
        }
        self.visible = library::matches(&self.games, &self.query);
        self.scan = None;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.receive_scan();
        let russian = self.russian;
        let text = |ru: &'static str, en: &'static str| if russian { ru } else { en };
        CentralPanel::default().show(ui, |ui| {
            ui.heading(text("Установленные игры", "Installed games"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(text("Папка Steam", "Steam folder"));
                let root = ui.add_enabled(
                    self.scan.is_none(),
                    TextEdit::singleline(&mut self.root).desired_width(280.0),
                );
                let enabled = self.scan.is_none() && !self.root.trim().is_empty();
                let button = ui.add_enabled(enabled, egui::Button::new(text("Читать", "Scan")));
                if button.clicked() || enabled && root.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    self.scan(ui.ctx());
                }
            });
            let search = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text(text("Поиск по названию или AppID", "Search by title or AppID"))
                    .desired_width(f32::INFINITY),
            );
            if search.changed() {
                self.visible = library::matches(&self.games, &self.query);
            }
            ui.separator();

            if self.scan.is_some() {
                ui.label(text("Чтение библиотеки…", "Reading library…"));
            } else if let Some(problem) = &self.problem {
                ui.label(text("Не удалось прочитать библиотеку", "Could not read the library"));
                ui.label(problem);
            } else if self.visible.is_empty() {
                ui.label(text("Игры не найдены", "No games found"));
            } else {
                ScrollArea::vertical().show_rows(ui, 24.0, self.visible.len(), |ui, range| {
                    for index in range {
                        let label = egui::Label::new(&self.games[self.visible[index]].name).truncate();
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
