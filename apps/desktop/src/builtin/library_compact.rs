use super::{Ctx, Dialog, Entry, Filter, Library, MainAction, MenuAct, Theme, Ui, is_shortcut};
use client::{Command, Link};
use i18n::t;

impl Library {
    pub(super) fn compact_page(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx) {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(t!("Search games")).desired_width(240.0));
            let mut installed = self.filter == Filter::Installed;
            if ui.checkbox(&mut installed, t!("Installed")).changed() {
                self.filter = if installed { Filter::Installed } else { Filter::All };
            }
            ui.checkbox(&mut self.favorites_only, t!("Favorites"));
        });
        ui.add_space(6.0);
        self.run_pending(ctx);
        let entries = self.compact_entries(ctx.state, ctx.local);
        if let Some(entry) = entries.iter().find(|e| Some(e.app_id) == self.selected).or_else(|| entries.first()) {
            if self.pending.is_none() && self.dialog.is_none() {
                self.selected = Some(entry.app_id);
            }
            self.compact_actions(ui, ctx, entry.app_id, entry.name);
            ui.separator();
            let row_height = (ui.text_style_height(&egui::TextStyle::Button) + ui.spacing().button_padding.y * 2.0).max(24.0);
            egui::ScrollArea::vertical().id_salt("compact-library").show_rows(ui, row_height, entries.len(), |ui, rows| {
                for entry in &entries[rows] {
                    ui.push_id(entry.app_id, |ui| {
                        let row = egui::Button::selectable(Some(entry.app_id) == self.selected, "").left_text(entry.name).truncate();
                        if ui.add_sized([ui.available_width(), row_height], row).on_hover_text(entry.name).clicked() {
                            self.selected = Some(entry.app_id);
                        }
                    });
                }
            });
        } else if ctx.state.apps.is_none() && self.search.is_empty() && ctx.state.link == Link::Online {
            ui.label(t!("Loading…"));
        } else {
            ui.label(t!("No games found"));
        }

        if matches!(self.dialog, Some(Dialog::Uninstall)) {
            if let Some(id) = self.selected {
                let game = ctx.local.games.iter().find(|g| g.app_id == id);
                let name = game.map_or("", |g| g.name.as_str());
                self.dialogs(ui, t, ctx, id, name, game);
            }
        }
        self.install_window(ui, t, ctx);
        self.sync_collections(ctx);
    }

    fn compact_entries<'a>(&self, state: &'a client::State, local: &'a local::LocalState) -> Vec<Entry<'a>> {
        let mut entries = self.filtered_in(state, local, self.filter);
        if self.favorites_only {
            entries.retain(|entry| self.favorites.contains(&entry.app_id));
        }
        entries
    }

    fn compact_actions(&mut self, ui: &mut Ui, ctx: &mut Ctx, id: model::AppId, name: &str) {
        let shortcut = is_shortcut(id);
        let action = if shortcut { MainAction::Play } else { self.main_action(ctx, id) };
        let online = ctx.state.link == Link::Online;
        let (label, request) = match action {
            MainAction::Play => (t!("Play"), Some(MenuAct::Play)),
            MainAction::Running => (t!("Running"), None),
            MainAction::Update if online => (t!("Update"), Some(MenuAct::Update)),
            MainAction::Update => (t!("Play"), Some(MenuAct::Play)),
            MainAction::Install => (t!("Install"), online.then_some(MenuAct::Install)),
            MainAction::Syncing => (t!("Syncing…"), None),
            MainAction::FirstTimeSetup => (t!("Performing first-time setup…"), None),
            MainAction::Queued => (t!("Queued"), None),
            MainAction::Uninstalling => (t!("Uninstalling…"), None),
            MainAction::Moving => (t!("Moving…"), None),
            MainAction::Busy { verifying: true, .. } => (t!("Verifying…"), None),
            MainAction::Busy { paused: true, .. } => (t!("Resume"), online.then_some(MenuAct::ResumeDownload)),
            MainAction::Busy { .. } => (t!("Pause"), Some(MenuAct::PauseDownload)),
            MainAction::StorePage => (t!("Not installed"), None),
        };
        ui.add(egui::Label::new(egui::RichText::new(name).strong().color(ctx.theme.text)).truncate());
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(request.is_some(), egui::Button::new(label)).clicked() {
                if let Some(request) = request {
                    self.apply_menu(ctx, id, request);
                }
            }
            let favorite = self.favorites.contains(&id);
            let label = if favorite { t!("Remove from favorites") } else { t!("Add to favorites") };
            if ui.selectable_label(favorite, label).clicked() {
                self.apply_menu(ctx, id, MenuAct::Favorite);
            }
            if !shortcut && matches!(action, MainAction::Play | MainAction::Update) && ui.button(t!("Uninstall")).clicked() {
                self.apply_menu(ctx, id, MenuAct::Uninstall);
            }
            match action {
                MainAction::Busy { fraction, verifying, .. } => {
                    ui.add(egui::ProgressBar::new(fraction).desired_width(110.0).show_percentage());
                    if ui.button(t!("Cancel")).clicked() {
                        if verifying {
                            ctx.send(Command::CancelVerify { app_id: id });
                        } else {
                            self.apply_menu(ctx, id, MenuAct::CancelDownload);
                        }
                    }
                }
                MainAction::Queued if ui.button(t!("Remove from download queue")).clicked() => {
                    self.apply_menu(ctx, id, MenuAct::CancelDownload);
                }
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_library_filters_owned_and_installed_games() {
        let mut state = client::State::default();
        state.apps = Some(
            [(1, "Гонка"), (2, "Гонка 2"), (3, "Стратегия")]
                .into_iter()
                .map(|(app_id, name)| (app_id, model::OwnedApp { app_id, name: name.into(), kind: "game".into(), ..Default::default() }))
                .collect(),
        );
        let installed = |app_id, name: &str| model::InstalledGame {
            app_id,
            name: name.into(),
            install_dir: Default::default(),
            library: Default::default(),
            size_on_disk: 0,
            last_updated: None,
            last_update_bytes: None,
            last_played: None,
            playtime_minutes: None,
            state: model::InstallState::Installed,
        };
        let local = local::LocalState { games: vec![installed(1, "Гонка"), installed(4, "Гонка 4")], ..Default::default() };
        let mut lib = Library { search: "ГОНКА".into(), favorites: [1, 2].into(), ..Default::default() };
        assert!(matches!(lib.view, super::super::View::List));
        let ids = |lib: &Library| lib.compact_entries(&state, &local).iter().map(|e| e.app_id).collect::<Vec<_>>();
        assert_eq!(ids(&lib), [1, 2, 4]);
        lib.filter = Filter::Installed;
        assert_eq!(ids(&lib), [1, 4]);
        lib.favorites_only = true;
        assert_eq!(ids(&lib), [1]);
    }
}
