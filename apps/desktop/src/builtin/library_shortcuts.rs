//! Non-Steam games and custom artwork, like Steam's.
//!
//! A non-Steam game gets Steam's own kind of id (a CRC of the program and
//! the name with the top bit set), so it lives in the same lists,
//! collections, favorites and hidden set as Steam games. Games the user
//! added in Steam (`userdata/<account>/config/shortcuts.vdf`) show up too.
//!
//! Custom art (icon, cover, background, logo, header) is copied into
//! SteamInRust's data folder. Art set in Steam (`config/grid/`) is used
//! when there's none of ours; Steam's files are only ever read.

use model::AppId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The top bit marks a non-Steam game's id, like Steam's.
pub const SHORTCUT_BIT: u32 = 0x8000_0000;

pub fn is_shortcut(id: AppId) -> bool {
    id & SHORTCUT_BIT != 0
}

/// Steam's id for a non-Steam game: CRC-32 of the program and the name, top
/// bit set (what Steam names its `config/grid` pictures after).
pub fn shortcut_id(exe: &str, name: &str) -> AppId {
    crc32(format!("{exe}{name}").as_bytes()) | SHORTCUT_BIT
}

/// CRC-32 (the zlib / PNG one), as Steam uses for these ids.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A game added by hand (not from Steam), like Steam's "Add a non-Steam game".
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct CustomGame {
    /// Its id (see [`shortcut_id`]); kept when it's renamed or moved.
    pub id: AppId,
    pub name: String,
    /// The program (an .exe, .lnk, .bat…), as Steam's "Target".
    pub exe: String,
    pub args: String,
    /// "Start in": the folder it runs in (the program's own when empty).
    pub start_dir: String,
    /// The icon file Steam has for it (an .ico, .png, .exe), if any.
    pub icon: String,
    /// Read from Steam's shortcuts.vdf.
    pub from_steam: bool,
    /// Last started from SteamInRust (Unix seconds).
    pub last_played: u64,
}

impl CustomGame {
    pub fn new(name: String, exe: String, args: String) -> Self {
        let id = shortcut_id(&exe, &name);
        Self { id, name, exe, args, ..Default::default() }
    }

    /// The folder it runs in.
    pub fn work_dir(&self) -> Option<PathBuf> {
        let d = self.start_dir.trim().trim_matches('"');
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
        Path::new(self.exe.trim().trim_matches('"')).parent().map(Path::to_path_buf)
    }

    /// Start it: programs directly; shortcuts (.lnk, .url) and scripts the
    /// way Windows opens them.
    pub fn launch(&self) -> Result<(), String> {
        let exe = PathBuf::from(self.exe.trim().trim_matches('"'));
        let ext = exe.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let args: Vec<&str> = self.args.split_whitespace().collect();
        let mut cmd = if cfg!(windows) && matches!(ext.as_str(), "lnk" | "url" | "bat" | "cmd") {
            // `start` follows a shortcut to its target and passes the options on.
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", ""]).arg(&exe);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x0800_0000); // no console window
            }
            c
        } else if cfg!(target_os = "macos") && ext == "app" {
            let mut c = std::process::Command::new("open");
            c.arg(&exe);
            if !args.is_empty() {
                c.arg("--args");
            }
            c
        } else {
            std::process::Command::new(&exe)
        };
        cmd.args(&args);
        if let Some(dir) = self.work_dir().filter(|d| d.is_dir()) {
            cmd.current_dir(dir);
        }
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Non-Steam games from Steam's `shortcuts.vdf` (binary KeyValues).
pub fn read_steam_shortcuts(path: &Path) -> Vec<CustomGame> {
    let Ok(data) = std::fs::read(path) else { return Vec::new() };
    let Ok(doc) = vdf::binary::parse(&data) else { return Vec::new() };
    let Some(list) = doc.obj("shortcuts") else { return Vec::new() };
    list.iter()
        .filter_map(|(_, v)| {
            let o = v.as_obj()?;
            let text = |k: &str| o.str(k).unwrap_or_default().trim().trim_matches('"').to_string();
            let name = text("AppName");
            let exe = text("Exe");
            if name.is_empty() || exe.is_empty() {
                return None;
            }
            // Steam keeps the id as a signed 32-bit number; older files have none.
            let id = o.str("appid").and_then(|s| s.parse::<i64>().ok()).map(|n| n as u32).filter(|n| is_shortcut(*n)).unwrap_or_else(|| shortcut_id(o.str("Exe").unwrap_or_default(), o.str("AppName").unwrap_or_default()));
            Some(CustomGame {
                id,
                name,
                exe,
                args: o.str("LaunchOptions").unwrap_or_default().to_string(),
                start_dir: text("StartDir"),
                icon: text("icon"),
                from_steam: true,
                last_played: o.str("LastPlayTime").and_then(|s| s.parse::<i64>().ok()).filter(|n| *n > 0).map(|n| n as u64).unwrap_or(0),
            })
        })
        .collect()
}

/// A kind of custom picture, like Steam's.
#[cfg(feature = "full")]
#[path = "library_artwork.rs"]
mod artwork;

#[cfg(feature = "full")]
pub use artwork::*;

/// A `&'static str` for a non-Steam game's name, so it can sit in the same
/// lists as Steam games' names. Each distinct name is kept once.
pub fn intern(s: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Option<std::collections::HashSet<&'static str>>> = std::sync::Mutex::new(None);
    let mut names = NAMES.lock().unwrap();
    let set = names.get_or_insert_with(Default::default);
    if let Some(n) = set.get(s) {
        return n;
    }
    let kept: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(kept);
    kept
}

// ---------- the library's side ----------

use super::{Library, MenuAct};
#[cfg(feature = "full")]
use super::props;
#[cfg(feature = "full")]
use egui::{Align, Align2, Color32, Layout, Margin, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use extensions::Ctx;
use i18n::t;
#[cfg(feature = "full")]
use ui::{Theme, icons, widgets as w};

impl Library {
    /// A non-Steam game by id.
    #[cfg(feature = "full")]
    pub(super) fn shortcut(&self, id: AppId) -> Option<&CustomGame> {
        is_shortcut(id).then(|| self.custom.iter().find(|g| g.id == id)).flatten()
    }

    /// When a non-Steam game was last started.
    #[cfg(feature = "full")]
    pub(super) fn shortcut_played(&self, id: AppId) -> Option<u64> {
        self.shortcut(id).map(|g| g.last_played).filter(|t| *t > 0)
    }

    /// A made-up non-Steam game for `--demo`.
    pub(super) fn demo_shortcut() -> CustomGame {
        CustomGame { last_played: super::now().saturating_sub(3 * 86_400), ..CustomGame::new("Starfall Arena".into(), "C:\\Games\\Starfall Arena\\starfall.exe".into(), "-windowed".into()) }
    }

    /// Find Steam's folder for this account (for its non-Steam games and
    /// custom art), and add the games added in Steam once it's known.
    pub(super) fn sync_steam_shortcuts(&mut self, ctx: &Ctx) {
        let root = &ctx.local.steam_root;
        let dir = if root.as_os_str().is_empty() {
            None
        } else {
            let account = if ctx.state.steam_id != 0 {
                Some(model::account_id(ctx.state.steam_id))
            } else {
                ctx.local.accounts.iter().find(|a| a.most_recent).or(ctx.local.accounts.first()).map(|a| model::account_id(a.steam_id))
            };
            account.map(|a| root.join("userdata").join(a.to_string()).join("config"))
        };
        #[cfg(feature = "full")]
        if !set_steam_config(dir.clone()) {
            return;
        }
        #[cfg(not(feature = "full"))]
        {
            if self.shortcut_config == dir {
                return;
            }
            self.shortcut_config = dir.clone();
        }
        let Some(dir) = dir else { return };
        for g in read_steam_shortcuts(&dir.join("shortcuts.vdf")) {
            if !self.removed_steam.contains(&g.id) && !self.custom.iter().any(|c| c.id == g.id) {
                self.custom.push(g);
            }
        }
    }

    /// Start a non-Steam game (the demo only says so).
    pub(super) fn launch_shortcut(&mut self, ctx: &mut Ctx, id: AppId) {
        let Some(g) = self.custom.iter_mut().find(|g| g.id == id) else { return };
        let result = if crate::app::demo() { Ok(()) } else { g.launch() };
        match result {
            Ok(()) => {
                g.last_played = super::now();
                ctx.toast(t!("Starting {name}…", name = g.name));
            }
            Err(e) => ctx.toast(t!("Couldn't start {name}: {error}", name = g.name, error = e)),
        }
    }

    /// Take a non-Steam game out of the library, with its pictures.
    #[cfg(feature = "full")]
    pub(super) fn remove_shortcut(&mut self, ctx: &mut Ctx, id: AppId) {
        let Some(i) = self.custom.iter().position(|g| g.id == id) else { return };
        let g = self.custom.remove(i);
        if g.from_steam {
            self.removed_steam.insert(id);
        }
        self.favorites.remove(&id);
        self.hidden.remove(&id);
        for c in &mut self.collections {
            c.games.remove(&id);
        }
        let _ = std::fs::remove_dir_all(art_root().join(id.to_string()));
        artwork::forget_found();
        if self.selected == Some(id) {
            self.selected = None;
        }
        ctx.toast(t!("{name} was removed from your library", name = g.name));
    }

    /// The right-click menu for a non-Steam game.
    #[cfg(feature = "full")]
    pub(super) fn shortcut_menu(&mut self, t: &Theme, resp: &egui::Response, id: AppId) {
        let mut pick = None;
        resp.context_menu(|ui| pick = self.shortcut_menu_items(ui, t, id));
        if let Some(act) = pick {
            self.menu_request = Some((id, act));
        }
    }

    /// The rows of a non-Steam game's menu; returns the one picked.
    #[cfg(feature = "full")]
    fn shortcut_menu_items(&self, ui: &mut Ui, t: &Theme, id: AppId) -> Option<MenuAct> {
        let fav = self.favorites.contains(&id);
        let hid = self.hidden.contains(&id);
        let pick = std::cell::Cell::new(None);
        {
            ui.set_min_width(220.0);
            let item = |ui: &mut Ui, icon: &str, label: &str, act: MenuAct| {
                if w::menu_item(ui, t, icon, label).clicked() {
                    pick.set(Some(act));
                    ui.close();
                }
            };
            item(ui, icons::PLAY, t!("Play"), MenuAct::Play);
            ui.separator();
            item(ui, icons::STAR, if fav { t!("Remove from favorites") } else { t!("Add to favorites") }, MenuAct::Favorite);
            item(ui, icons::EYE, if hid { t!("Unhide") } else { t!("Hide this game") }, MenuAct::Hide);
            let r = w::submenu_item(ui, t, icons::GEAR, t!("Manage"));
            egui::containers::menu::SubMenu::new().show(ui, &r, |ui| {
                ui.set_min_width(220.0);
                item(ui, icons::DESKTOP, t!("Add desktop shortcut"), MenuAct::Shortcut);
                item(ui, icons::FOLDER, t!("Browse local files"), MenuAct::Browse);
                item(ui, icons::IMAGE, t!("Set custom artwork…"), MenuAct::CustomArt);
            });
            let r = w::submenu_item(ui, t, icons::STACK, t!("Add to collection"));
            egui::containers::menu::SubMenu::new().show(ui, &r, |ui| {
                ui.set_min_width(220.0);
                for (i, c) in self.collections.iter().enumerate() {
                    let on = c.games.contains(&id);
                    if w::menu_item(ui, t, if on { icons::CHECK } else { "" }, &c.name).clicked() {
                        pick.set(Some(MenuAct::ToggleCollection(i)));
                        ui.close();
                    }
                }
                if !self.collections.is_empty() {
                    ui.separator();
                }
                item(ui, icons::PLUS, t!("New collection…"), MenuAct::NewCollection);
            });
            ui.separator();
            item(ui, icons::SLIDERS, t!("Properties…"), MenuAct::Properties);
            if w::menu_item_tinted(ui, t, icons::TRASH, t!("Remove from library"), Some(t.danger)).clicked() {
                pick.set(Some(MenuAct::RemoveShortcut));
                ui.close();
            }
        }
        pick.get()
    }

    /// What a menu choice means for a non-Steam game; false for the ones
    /// that work the same as for any game (favorites, collections…).
    pub(super) fn apply_shortcut_menu(&mut self, ctx: &mut Ctx, id: AppId, act: MenuAct) -> bool {
        #[cfg(feature = "full")]
        let g = self.shortcut(id).cloned();
        match act {
            MenuAct::Play => self.launch_shortcut(ctx, id),
            #[cfg(feature = "full")]
            MenuAct::Browse => {
                if let Some(dir) = g.and_then(|g| g.work_dir()).filter(|d| d.is_dir()) {
                    local::open_folder(&dir);
                }
            }
            #[cfg(feature = "full")]
            MenuAct::Shortcut => {
                if let Some(g) = g {
                    let exe = PathBuf::from(g.exe.trim().trim_matches('"'));
                    let icon = exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")).then_some(exe);
                    match crate::links::desktop_shortcut(id, &g.name, icon) {
                        Ok(_) => ctx.toast(t!("Shortcut added to your desktop")),
                        Err(e) => ctx.toast(e),
                    }
                }
            }
            #[cfg(feature = "full")]
            MenuAct::Properties => self.props = Some(props::Props { app: id, tab: props::PropTab::Shortcut, ..Default::default() }),
            #[cfg(feature = "full")]
            MenuAct::CustomArt => self.props = Some(props::Props { app: id, tab: props::PropTab::Customization, ..Default::default() }),
            #[cfg(feature = "full")]
            MenuAct::RemoveShortcut => self.remove_shortcut(ctx, id),
            _ => return false,
        }
        true
    }

    /// The background at the top of a game's page, fading into the page,
    /// with the logo (where it was put) or the name. Right-click it to set
    /// custom pictures, like Steam.
    #[cfg(feature = "full")]
    pub(super) fn hero(&mut self, ui: &mut Ui, t: &Theme, id: AppId, name: &str, side: f32) {
        let full = ui.available_width();
        let (hero, resp) = ui.allocate_exact_size(vec2(full, t.dense(330.0)), Sense::click());
        let area = Rect::from_min_size(hero.min, vec2(full, hero.height() + t.spacing.xl * 4.0));
        let (hero_url, header_url) = (super::shortcuts::hero(id), super::shortcuts::header(id));
        let urls: Vec<&str> = [hero_url.as_str(), header_url.as_str()].into_iter().filter(|u| !u.is_empty()).collect();
        if urls.is_empty() {
            // No picture yet: a soft wash of the theme's colors.
            w::gradient(&ui.painter_at(area), area, t.accent_2.gamma_multiply(0.4), Color32::TRANSPARENT, false);
        } else if !w::paint_cover_fade(ui, area, &urls, 0.6) {
            w::skeleton(ui, t, hero, egui::CornerRadius::ZERO);
        }
        let (logo, align) = match logo_position(id) {
            Some(pos) => (logo_rect(hero, pos, side + t.spacing.xl), pin_align(pos.pin)),
            None => (Rect::from_min_max(hero.left_top() + vec2(side + t.spacing.xl, t.spacing.xl * 2.0), hero.left_bottom() + vec2(side + 440.0, -t.spacing.xl * 2.5)), Align2::LEFT_BOTTOM),
        };
        let logo_url = super::shortcuts::logo(id);
        if logo_url.is_empty() || !w::paint_contain(ui, logo, &logo_url, align) {
            let size = t.typography.heading_l.size * 1.5;
            let at = align.pos_in_rect(&logo);
            ui.painter().text(at, align, name, egui::FontId::new(size, t.typography.heading_l.weight.family()), t.text);
        }
        let has = |k: ArtKind| custom_art(id, k).is_some();
        let pick = std::cell::Cell::new(None);
        resp.context_menu(|ui| {
            ui.set_min_width(240.0);
            let mut item = |icon: &str, label: &str, act: u8| {
                if w::menu_item(ui, t, icon, label).clicked() {
                    pick.set(Some(act));
                    ui.close();
                }
            };
            item(icons::IMAGE, t!("Set custom background…"), 0);
            item(icons::IMAGE, t!("Set custom logo…"), 1);
            item(icons::SLIDERS, t!("Adjust logo position…"), 2);
            if has(ArtKind::Hero) {
                item(icons::X, t!("Clear custom background"), 3);
            }
            if has(ArtKind::Logo) {
                item(icons::X, t!("Clear custom logo"), 4);
            }
        });
        match pick.get() {
            Some(0) => props::pick_art(ui.ctx(), id, ArtKind::Hero),
            Some(1) => props::pick_art(ui.ctx(), id, ArtKind::Logo),
            Some(2) => {
                let tab = props::PropTab::Customization;
                self.props = Some(props::Props { app: id, tab, ..Default::default() });
            }
            Some(3) => clear_art(id, ArtKind::Hero),
            Some(4) => clear_art(id, ArtKind::Logo),
            _ => {}
        }
    }

    /// A non-Steam game's page, laid out like a Steam game's: the
    /// background, the play bar, then its details, artwork and notes.
    #[cfg(feature = "full")]
    pub(super) fn shortcut_page(&mut self, id: AppId, ui: &mut Ui, t: &Theme, ctx: &mut Ctx) {
        let Some(g) = self.shortcut(id).cloned() else {
            self.selected = None;
            return;
        };
        let side = (t.spacing.xl + t.spacing.sm + 2.0).round();
        egui::ScrollArea::vertical().id_salt(("shortcut", id)).auto_shrink([false, false]).show(ui, |ui| {
            self.hero(ui, t, id, &g.name, side);
            egui::Frame::new().inner_margin(Margin { left: (side - 10.0) as i8 + 18, right: side as i8, top: 0, bottom: 0 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.shortcut_bar(ui, t, ctx, &g);
            });
            ui.add_space(t.spacing.xl);
            egui::Frame::new().inner_margin(Margin::symmetric(side as i8 - 9, 0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = t.spacing.lg;
                let total = ui.available_width();
                let side_w = (total * 0.37).clamp(300.0, 560.0);
                if total >= 820.0 {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = t.spacing.xl - 3.0;
                        ui.vertical(|ui| {
                            ui.set_width(total - side_w - t.spacing.xl + 3.0);
                            ui.spacing_mut().item_spacing.y = t.spacing.lg;
                            self.shortcut_details(ui, t, ctx, &g);
                            self.notes_card(ui, t, id);
                        });
                        ui.vertical(|ui| {
                            ui.set_width(side_w);
                            self.artwork_card(ui, t, id);
                        });
                    });
                } else {
                    self.shortcut_details(ui, t, ctx, &g);
                    self.artwork_card(ui, t, id);
                    self.notes_card(ui, t, id);
                }
                ui.add_space(t.spacing.xl * 2.0);
            });
        });
    }

    /// Name, Play, last played; favorite, Properties and the menu at the right.
    #[cfg(feature = "full")]
    fn shortcut_bar(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx, g: &CustomGame) {
        let id = g.id;
        let row_h = (t.height_of(&t.components.button_play, 40.0) * 1.325).round();
        let (rr, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
        let mut right = ui.new_child(UiBuilder::new().max_rect(rr).layout(Layout::right_to_left(Align::Center)));
        right.spacing_mut().item_spacing.x = t.spacing.md - 1.0;
        let more = w::icon_box(&mut right, t, icons::INFO, t!("Manage"), false);
        let mut pick = None;
        egui::Popup::menu(&more).width(240.0).show(|ui| pick = self.shortcut_menu_items(ui, t, id));
        if let Some(act) = pick {
            self.menu_request = Some((id, act));
        }
        if w::icon_box(&mut right, t, icons::GEAR, t!("Properties…"), false).clicked() {
            self.props = Some(props::Props { app: id, tab: props::PropTab::Shortcut, ..Default::default() });
        }
        let fav = self.favorites.contains(&id);
        if w::icon_box(&mut right, t, icons::STAR, if fav { t!("Remove from favorites") } else { t!("Add to favorites") }, fav).clicked() && !self.favorites.remove(&id) {
            self.favorites.insert(id);
        }
        let left = Rect::from_min_max(rr.min, pos2(right.min_rect().left() - t.spacing.xl, rr.bottom()));
        if left.width() < 120.0 {
            return;
        }
        let mut c = ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)));
        c.set_clip_rect(c.clip_rect().intersect(left));
        c.spacing_mut().item_spacing.x = t.spacing.xl + 3.0;
        let name_w = ui.painter().layout_no_wrap(g.name.clone(), t.typography.heading_m.font(), t.text).size().x;
        let title_w = name_w.min((left.width() * 0.34).max(120.0)).min(360.0);
        c.allocate_ui_with_layout(vec2(title_w, row_h), Layout::left_to_right(Align::Center), |ui| {
            ui.add(egui::Label::new(w::heading_m(t, &g.name, t.text)).truncate()).on_hover_text(&g.name);
        });
        if w::play_button(&mut c, t, t!("Play")).clicked() {
            self.launch_shortcut(ctx, id);
        }
        let last = if g.last_played > 0 { ui::date(g.last_played) } else { t!("Never").to_string() };
        super::views::icon_stat(&mut c, t, icons::CALENDAR, t!("Last played"), &last, None);
        if c.available_width() > 200.0 {
            super::views::icon_stat(&mut c, t, icons::CONTROLLER, t!("Source"), if g.from_steam { t!("Added in Steam") } else { t!("Non-Steam game") }, None);
        }
    }

    /// The program, its folder and options, with Properties and Browse.
    #[cfg(feature = "full")]
    fn shortcut_details(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx, g: &CustomGame) {
        let id = g.id;
        let _ = ctx;
        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::body_l(t, t!("Shortcut"), t.text));
            ui.add_space(t.spacing.xs);
            let dir = g.work_dir().map(|d| d.display().to_string()).unwrap_or_default();
            egui::Grid::new(("shortcut-details", id)).num_columns(2).spacing(vec2(t.spacing.lg, t.spacing.sm)).show(ui, |ui| {
                for (label, value) in [(t!("Target"), g.exe.as_str()), (t!("Start in"), dir.as_str()), (t!("Launch options"), g.args.as_str())] {
                    ui.label(w::caption_text(t, label));
                    let shown = if value.trim().is_empty() { "—" } else { value };
                    ui.add(egui::Label::new(w::body_m(t, shown, t.text)).truncate()).on_hover_text(value);
                    ui.end_row();
                }
            });
            ui.add_space(t.spacing.sm);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(t.spacing.sm, t.spacing.sm);
                if w::button(ui, t, Some(icons::SLIDERS), t!("Properties…"), w::Kind::Secondary, w::Size::Sm).clicked() {
                    self.props = Some(props::Props { app: id, tab: props::PropTab::Shortcut, ..Default::default() });
                }
                if let Some(dir) = g.work_dir().filter(|d| d.is_dir()) {
                    if w::button(ui, t, Some(icons::FOLDER), t!("Browse local files"), w::Kind::Ghost, w::Size::Sm).clicked() {
                        local::open_folder(&dir);
                    }
                }
            });
        });
    }

    /// The game's custom pictures, each with Change and Clear.
    #[cfg(feature = "full")]
    pub(super) fn artwork_card(&mut self, ui: &mut Ui, t: &Theme, id: AppId) {
        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::body_l(t, t!("Artwork"), t.text));
            ui.add_space(t.spacing.xs);
            for kind in ArtKind::ALL {
                let label = match kind {
                    ArtKind::Capsule => t!("Cover"),
                    ArtKind::Hero => t!("Background"),
                    ArtKind::Logo => t!("Logo"),
                    ArtKind::Header => t!("Wide header"),
                    ArtKind::Icon => t!("Icon"),
                };
                let current = custom_art(id, kind);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(64.0, 40.0), Sense::hover());
                    let shown = current.as_deref().is_some_and(|u| if kind == ArtKind::Logo { w::paint_contain(ui, r, u, Align2::CENTER_CENTER) } else { w::paint_cover(ui, r, u, t.corner_small()) });
                    if !shown {
                        ui.painter().rect_filled(r, t.corner_small(), t.surface_hover);
                        w::icon_centered(ui.painter(), r.center(), icons::IMAGE, 18.0, t.text_faint);
                    }
                    ui.label(w::body_m(t, label, t.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if current.is_some() && w::icon_button(ui, t, icons::X, t!("Clear"), false, 0).clicked() {
                            clear_art(id, kind);
                        }
                        if w::icon_button(ui, t, icons::IMAGE, t!("Change…"), false, 0).clicked() {
                            props::pick_art(ui.ctx(), id, kind);
                        }
                    });
                });
            }
        });
    }

    /// A note for yourself about the game.
    #[cfg(feature = "full")]
    fn notes_card(&mut self, ui: &mut Ui, t: &Theme, id: AppId) {
        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::body_l(t, t!("Notes"), t.text));
            let s = self.games.entry(id).or_default();
            ui.add(egui::TextEdit::multiline(&mut s.notes).hint_text(t!("Write a note for yourself…")).desired_rows(3).desired_width(f32::INFINITY));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary KeyValues file like Steam writes for shortcuts.
    fn shortcuts_vdf(items: &[(&str, &str, i32, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        let key = |out: &mut Vec<u8>, kind: u8, k: &str| {
            out.push(kind);
            out.extend_from_slice(k.as_bytes());
            out.push(0);
        };
        let s = |out: &mut Vec<u8>, k: &str, v: &str| {
            key(out, 1, k);
            out.extend_from_slice(v.as_bytes());
            out.push(0);
        };
        key(&mut out, 0, "shortcuts");
        for (i, (name, exe, appid, opts)) in items.iter().enumerate() {
            key(&mut out, 0, &i.to_string());
            key(&mut out, 2, "appid");
            out.extend_from_slice(&appid.to_le_bytes());
            s(&mut out, "AppName", name);
            s(&mut out, "Exe", &format!("\"{exe}\""));
            s(&mut out, "StartDir", "\"C:\\Games\\\"");
            s(&mut out, "icon", "");
            s(&mut out, "LaunchOptions", opts);
            key(&mut out, 2, "LastPlayTime");
            out.extend_from_slice(&1_700_000_000i32.to_le_bytes());
            key(&mut out, 0, "tags");
            out.push(8);
            out.push(8);
        }
        out.push(8);
        out.push(8);
        out
    }

    #[test]
    fn reads_steams_shortcuts() {
        let dir = std::env::temp_dir().join(format!("sir-shortcuts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shortcuts.vdf");
        std::fs::write(&path, shortcuts_vdf(&[("Tenkaichi 3", "C:\\Games\\bt3.lnk", -1_500_000_000, "-fullscreen")])).unwrap();
        let games = read_steam_shortcuts(&path);
        assert_eq!(games.len(), 1);
        let g = &games[0];
        assert_eq!(g.name, "Tenkaichi 3");
        assert_eq!(g.exe, "C:\\Games\\bt3.lnk");
        assert_eq!(g.start_dir, "C:\\Games\\");
        assert_eq!(g.args, "-fullscreen");
        assert_eq!(g.id, (-1_500_000_000i32) as u32);
        assert!(is_shortcut(g.id) && g.from_steam && g.last_played == 1_700_000_000);
        // Missing or broken files give nothing, never a panic.
        assert!(read_steam_shortcuts(&dir.join("none.vdf")).is_empty());
        std::fs::write(&path, b"\x00shortcuts\x00\x00").unwrap();
        assert!(read_steam_shortcuts(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ids_are_steams() {
        let id = shortcut_id("\"C:\\Games\\game.exe\"", "Game");
        assert!(is_shortcut(id));
        assert_eq!(id, shortcut_id("\"C:\\Games\\game.exe\"", "Game"));
        assert_ne!(id, shortcut_id("\"C:\\Games\\game.exe\"", "Game 2"));
        assert!(!is_shortcut(570));
        // The standard check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
