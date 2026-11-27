use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage, TextureHandle};
use net::{Connection, Packet, emsg, eresult};
use proto::*;
use tokio::sync::{mpsc, oneshot};

use crate::{catalog, download, install, library::Game};

const SERVICE: &str = "dev.SteamUltraLite";
const DEVICE: &str = "SteamUltraLite";
const PROTOCOL_VERSION: u32 = 65580;
const OS_TYPE_WIN11: u32 = 20;
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum Mode { Qr { remember: bool }, Saved }

#[derive(Clone, Copy)]
enum Failure { Connect, Confirm, Logon, Library, Credentials, Disconnected }

type SessionResult<T = ()> = Result<T, Failure>;

enum Event {
    Qr(ColorImage),
    Loading,
    Library(Vec<Game>, mpsc::UnboundedSender<Command>),
    Installed(u32, Result<(), String>),
    Finished(SessionResult),
}

enum Command {
    Install { app: model::OwnedApp, library: PathBuf, russian: bool, progress: Arc<depot::Progress> },
}

enum State {
    SignedOut,
    Connecting,
    Qr(TextureHandle),
    Loading,
    SignedIn,
    Closing,
    Failed(Failure),
}

pub struct Account {
    games: Vec<Game>,
    state: State,
    remember: bool,
    worker: Option<(Receiver<Event>, Option<oneshot::Sender<bool>>)>,
    commands: Option<mpsc::UnboundedSender<Command>>,
    progress: Option<Arc<depot::Progress>>,
    install_id: Option<u32>,
    install_result: Option<String>,
    installed_id: Option<u32>,
    install_russian: bool,
}

impl Default for Account {
    fn default() -> Self {
        Self { games: Vec::new(), state: State::SignedOut, remember: false, worker: None, commands: None, progress: None, install_id: None, install_result: None, installed_id: None, install_russian: false }
    }
}

impl Account {
    fn start(&mut self, mode: Mode, ctx: &egui::Context) {
        let (updates, receiver) = channel();
        let (stop, cancelled) = oneshot::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let status = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Failure::Connect)
                .and_then(|runtime| runtime.block_on(run(mode, cancelled, &updates, &repaint)));
            emit(&updates, &repaint, Event::Finished(status));
        });

        self.worker = Some((receiver, Some(stop)));
        self.state = State::Connecting;
    }

    fn stop(&mut self, forget: bool) {
        if let Some((_, stop)) = &mut self.worker
            && let Some(stop) = stop.take()
        {
            let _ = stop.send(forget);
        }
        self.state = State::Closing;
    }

    pub fn is_signed_in(&self) -> bool {
        matches!(self.state, State::SignedIn)
    }

    pub fn games(&self) -> &[Game] {
        &self.games
    }

    pub fn install(&mut self, game: &Game, library: PathBuf, russian: bool) {
        let Some(app) = game.content.clone() else { return };
        let Some(commands) = &self.commands else { return };
        if self.progress.is_some() { return; }
        let progress = Arc::new(depot::Progress::default());
        progress.concurrency.store(4, Ordering::Relaxed);
        if commands.send(Command::Install { app, library, russian, progress: progress.clone() }).is_ok() {
            self.install_russian = russian;
            self.install_id = Some(game.id);
            self.install_result = None;
            self.progress = Some(progress);
        }
    }

    pub fn take_installed(&mut self) -> Option<u32> {
        self.installed_id.take()
    }

    pub fn installing(&self) -> bool {
        self.progress.is_some()
    }

    pub fn install_ui(&mut self, ui: &mut egui::Ui, russian: bool) {
        if let Some(progress) = &self.progress {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
            let id = self.install_id.unwrap_or(0);
            let done = progress.downloaded.load(Ordering::Relaxed);
            let total = progress.download_total.load(Ordering::Relaxed);
            ui.label(if russian { format!("Загрузка {id}: {done}/{total} байт") } else { format!("Download {id}: {done}/{total} bytes") });
            if total > 0 {
                ui.add(egui::ProgressBar::new((done as f32 / total as f32).clamp(0.0, 1.0)));
            }
            let paused = progress.paused.load(Ordering::Relaxed);
            if ui.button(if paused { if russian { "Продолжить" } else { "Resume" } } else if russian { "Пауза" } else { "Pause" }).clicked() {
                progress.paused.store(!paused, Ordering::Relaxed);
            }
            if ui.button(if russian { "Отменить загрузку" } else { "Cancel download" }).clicked() {
                progress.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some(message) = &self.install_result {
            ui.label(message);
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let Some((updates, _)) = &self.worker else { return false };
        let mut changed = false;
        loop {
            let event = match updates.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => return changed,
                Err(TryRecvError::Disconnected) => Event::Finished(Err(Failure::Disconnected)),
            };
            match event {
                Event::Finished(result) => {
                    changed = true;
                    self.games.clear();
                    self.commands = None;
                    self.progress = None;
                    self.install_id = None;
                    self.worker = None;
                    self.state = match result {
                        Ok(()) => State::SignedOut,
                        Err(error) => State::Failed(error),
                    };
                    return changed;
                }
                _ if matches!(self.state, State::Closing) => {}
                Event::Qr(image) => self.state = State::Qr(ctx.load_texture("Steam Guard", image, egui::TextureOptions::NEAREST)),
                Event::Loading => self.state = State::Loading,
                Event::Installed(id, result) => {
                    self.progress = None;
                    self.install_id = None;
                    self.install_result = Some(match result {
                        Ok(()) => {
                            self.installed_id = Some(id);
                            if self.install_russian { "Установка завершена" } else { "Installation complete" }.into()
                        }
                        Err(_) => if self.install_russian { "Установка остановлена или не удалась; проверь папку и повтори" } else { "Install stopped or failed; check the folder and retry" }.into(),
                    });
                }
                Event::Library(games, commands) => {
                    self.commands = Some(commands);
                    self.games = games;
                    self.state = State::SignedIn;
                    changed = true;
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, russian: bool) {
        let text = |ru: &'static str, en: &'static str| if russian { ru } else { en };
        match &self.state {
            State::SignedOut | State::Failed(_) => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button(text("Войти по QR", "Sign in with QR")).clicked() {
                        self.start(Mode::Qr { remember: self.remember }, ui.ctx());
                    }
                    if ui.button(text("Сохранённый вход", "Use saved sign-in")).clicked() {
                        self.start(Mode::Saved, ui.ctx());
                    }
                    ui.checkbox(&mut self.remember, text("Запомнить вход", "Remember sign-in"));
                });
                if let State::Failed(error) = self.state {
                    ui.label(match error {
                        Failure::Connect => text("Не удалось подключиться к Steam", "Could not connect to Steam"),
                        Failure::Confirm => text("QR-код истёк или не принят Steam", "The QR code expired or Steam refused it"),
                        Failure::Logon => text("Steam отклонил вход", "Steam rejected the sign-in"),
                        Failure::Library => text("Не удалось получить библиотеку Steam", "Could not load your Steam library"),
                        Failure::Credentials => text("Сохранённый вход недоступен", "The saved sign-in is unavailable"),
                        Failure::Disconnected => text("Соединение со Steam потеряно", "Connection to Steam was lost"),
                    });
                }
            }
            State::Connecting | State::Loading => {
                let loading = matches!(self.state, State::Loading);
                ui.horizontal(|ui| {
                    ui.label(if loading {
                        text("Получение библиотеки Steam…", "Loading your Steam library…")
                    } else {
                        text("Подключение к Steam…", "Connecting to Steam…")
                    });
                    if ui.button(text("Отмена", "Cancel")).clicked() {
                        self.stop(false);
                    }
                });
            }
            State::Qr(texture) => {
                ui.label(text("Отсканируй код в Steam Guard на телефоне", "Scan this code with Steam Guard on your phone"));
                ui.add(egui::Image::new((texture.id(), egui::vec2(192.0, 192.0))));
                if ui.button(text("Отмена", "Cancel")).clicked() {
                    self.stop(false);
                }
            }
            State::SignedIn => {
                if ui.button(text("Выйти из аккаунта", "Sign out")).clicked() {
                    self.stop(true);
                }
            }
            State::Closing => { ui.label(text("Закрытие сессии…", "Closing session…")); }
        }
    }
}

fn emit(updates: &Sender<Event>, ctx: &egui::Context, event: Event) {
    let _ = updates.send(event);
    ctx.request_repaint();
}

async fn run(
    mode: Mode,
    mut cancel: oneshot::Receiver<bool>,
    updates: &Sender<Event>,
    ctx: &egui::Context,
) -> SessionResult {
    let (conn, mut events, account, heartbeat, cell_id) = tokio::select! {
        result = authorize(mode, updates, ctx) => result?,
        _ = &mut cancel => return Ok(()),
    };
    let maintain = conn.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(heartbeat);
        loop {
            ticker.tick().await;
            if maintain.send(emsg::CLIENT_HEARTBEAT, &CMsgClientHeartBeat::default()).is_err() {
                break;
            }
        }
    });
    emit(updates, ctx, Event::Loading);
    let games = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(180), load_library(&conn, &mut events)) => {
            result.map_err(|_| Failure::Library)??
        }
        _ = &mut cancel => return Ok(()),
    };
    if matches!(mode, Mode::Qr { remember: true }) {
        save(&account)?;
    }
    let owned: HashSet<u32> = games.iter().map(|game| game.id).collect();
    let (commands, mut requests) = mpsc::unbounded_channel();
    emit(updates, ctx, Event::Library(games, commands));
    let outcome = loop {
        tokio::select! {
            forget = &mut cancel => break forget.unwrap_or(false),
            _ = wait_event(&mut events, emsg::CLIENT_LOGGED_OFF) => return Err(Failure::Disconnected),
            Some(Command::Install { app, library, russian, progress }) = requests.recv() => {
                let id = app.app_id;
                let result = match install::request(&app, &library, russian) {
                    Ok(request) if owned.contains(&id) => {
                        let downloaded = tokio::select! {
                            result = download::run(conn.clone(), app, owned.clone(), request, cell_id, None, account.steam_id, progress.clone()) => result.map(|_| ()),
                            _ = wait_event(&mut events, emsg::CLIENT_LOGGED_OFF) => return Err(Failure::Disconnected),
                            remove_saved = &mut cancel => {
                                progress.stop.store(true, Ordering::Relaxed);
                                let _ = conn.send(emsg::CLIENT_LOG_OFF, &CMsgClientLogOff::default());
                                return if remove_saved.unwrap_or(false) { forget() } else { Ok(()) };
                            }
                        };
                        downloaded
                    }
                    Ok(_) => Err("This account does not own the game".into()),
                    Err(error) => Err(error),
                };
                emit(updates, ctx, Event::Installed(id, result));
            }
        }
    };
    heartbeat_task.abort();
    let _ = conn.send(emsg::CLIENT_LOG_OFF, &CMsgClientLogOff::default());
    let _ = tokio::time::timeout(Duration::from_secs(2), wait_event(&mut events, emsg::CLIENT_LOGGED_OFF)).await;
    if outcome {
        forget()?;
    }
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SavedAccount {
    name: String,
    steam_id: u64,
    refresh_token: String,
}

impl SavedAccount {
    fn valid(&self) -> bool {
        !self.name.is_empty() && self.steam_id != 0 && !self.refresh_token.is_empty()
    }
}

fn credential() -> SessionResult<keyring::Entry> {
    keyring::Entry::new(SERVICE, "steam-refresh-session").map_err(|_| Failure::Credentials)
}

fn load() -> SessionResult<SavedAccount> {
    let value = credential()?.get_password().map_err(|_| Failure::Credentials)?;
    serde_json::from_str(&value).map_err(|_| Failure::Credentials)
}

fn save(account: &SavedAccount) -> SessionResult {
    let value = serde_json::to_string(account).map_err(|_| Failure::Credentials)?;
    credential()?.set_password(&value).map_err(|_| Failure::Credentials)
}

fn forget() -> SessionResult {
    match credential()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(Failure::Credentials),
    }
}

async fn authorize(
    mode: Mode,
    updates: &Sender<Event>,
    ctx: &egui::Context,
) -> SessionResult<(Connection, net::connection::Events, SavedAccount, Duration, u32)> {
    let saved = match mode {
        Mode::Saved => Some(load()?),
        Mode::Qr { .. } => None,
    };
    let (conn, mut events) = connect().await?;
    conn.send(emsg::CLIENT_HELLO, &CMsgClientHello { protocol_version: Some(PROTOCOL_VERSION) })
        .map_err(|_| Failure::Connect)?;
    let account = match saved {
        Some(account) => account,
        None => tokio::time::timeout(Duration::from_secs(180), qr_login(&conn, updates, ctx))
            .await.map_err(|_| Failure::Confirm)??,
    };
    if !account.valid() {
        return Err(Failure::Logon);
    }
    conn.set_session(account.steam_id, 0);
    conn.send(emsg::CLIENT_LOGON, &CMsgClientLogon {
        protocol_version: Some(PROTOCOL_VERSION),
        client_os_type: Some(OS_TYPE_WIN11),
        client_language: Some("english".into()),
        should_remember_password: Some(matches!(mode, Mode::Saved | Mode::Qr { remember: true })),
        account_name: Some(account.name.clone()),
        access_token: Some(account.refresh_token.clone()),
        machine_name: Some(DEVICE.into()),
        chat_mode: Some(2),
        obfuscated_private_ip: Some(CMsgIpAddress {
            ip: Some(c_msg_ip_address::Ip::V4((u32::from_str_radix(&auth::random_hex(4), 16).unwrap_or(1) ^ 0xBAAD_F00D).max(1))),
        }),
        ..Default::default()
    }).map_err(|_| Failure::Logon)?;
    let reply = tokio::time::timeout(DEADLINE, wait_event(&mut events, emsg::CLIENT_LOGON_RESPONSE))
        .await.map_err(|_| Failure::Logon)??;
    let response: CMsgClientLogonResponse = reply.body().map_err(|_| Failure::Logon)?;
    if response.eresult() != eresult::OK {
        return Err(Failure::Logon);
    }
    let session = reply.session_for(account.steam_id).ok_or(Failure::Logon)?;
    conn.set_session(account.steam_id, session);
    let heartbeat = Duration::from_secs(response.heartbeat_seconds().clamp(5, 60) as u64);
    Ok((conn, events, account, heartbeat, response.cell_id()))
}

async fn connect() -> SessionResult<(Connection, net::connection::Events)> {
    let servers = net::directory::websocket_servers().await.map_err(|_| Failure::Connect)?;
    for endpoint in servers.iter().filter(|item| item.ends_with(":443")).take(6) {
        if let Ok(Ok(session)) = tokio::time::timeout(Duration::from_secs(8), Connection::connect_steam(endpoint)).await {
            return Ok(session);
        }
    }
    Err(Failure::Connect)
}

async fn qr_login(conn: &Connection, updates: &Sender<Event>, ctx: &egui::Context) -> SessionResult<SavedAccount> {
    let mut pending = auth::begin_qr(conn, &auth::Device { name: DEVICE.into() }).await.map_err(|_| Failure::Confirm)?;
    let mut shown = String::new();
    loop {
        let url = pending.challenge_url.as_deref().ok_or(Failure::Confirm)?;
        if url != shown {
            let image = qr_image(url).ok_or(Failure::Confirm)?;
            emit(updates, ctx, Event::Qr(image));
            shown = url.to_owned();
        }
        tokio::time::sleep(pending.interval).await;
        if let Some(tokens) = pending.poll(conn).await.map_err(|_| Failure::Confirm)? {
            return Ok(SavedAccount { name: tokens.account_name, steam_id: tokens.steam_id, refresh_token: tokens.refresh_token });
        }
    }
}

fn qr_image(url: &str) -> Option<ColorImage> {
    let path = url.strip_prefix("https://s.team/q/")?;
    if path.is_empty() || path.len() > 100
        || path.split('/').any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    {
        return None;
    }
    let code = qrcode::QrCode::new(url).ok()?;
    let width = code.width();
    let edge = width + 8;
    let mut pixels = vec![Color32::WHITE; edge * edge];
    for (index, color) in code.into_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            pixels[(index / width + 4) * edge + index % width + 4] = Color32::BLACK;
        }
    }
    Some(ColorImage::new([edge, edge], pixels))
}

async fn wait_event(events: &mut net::connection::Events, emsg: u32) -> SessionResult<Packet> {
    while let Some(packet) = events.recv().await {
        if packet.emsg == emsg {
            return Ok(packet);
        }
        if packet.emsg == emsg::CLIENT_LOGGED_OFF {
            break;
        }
    }
    Err(Failure::Disconnected)
}

async fn load_library(conn: &Connection, events: &mut net::connection::Events) -> SessionResult<Vec<Game>> {
    let reply = wait_event(events, emsg::CLIENT_LICENSE_LIST).await?;
    let list: CMsgClientLicenseList = reply.body().map_err(|_| Failure::Library)?;
    if list.eresult() != eresult::OK {
        return Err(Failure::Library);
    }
    let packages: Vec<_> = list.licenses.iter().map(|license| (license.package_id(), license.access_token())).collect();
    let mut games = catalog::resolve(conn, &packages).await.map_err(|_| Failure::Library)?;
    let request = CPlayerGetOwnedGamesRequest {
        steamid: Some(conn.steam_id()),
        include_appinfo: Some(false),
        include_played_free_games: Some(true),
        ..Default::default()
    };
    if let Ok(Ok(owned)) = tokio::time::timeout(Duration::from_secs(6),
        conn.call::<_, CPlayerGetOwnedGamesResponse>("Player.GetOwnedGames#1", &request)).await
    {
        let times: std::collections::HashMap<_, _> = owned.games.iter()
            .map(|item| (item.appid() as u32, item.playtime_forever().max(0) as u64)).collect();
        for game in &mut games {
            game.playtime_minutes = times.get(&game.id).copied();
        }
    }
    for chunk in games.chunks_mut(100) {
        let request = CPlayerGetAchievementsProgressRequest {
            steamid: Some(conn.steam_id()),
            language: Some("english".into()),
            appids: chunk.iter().map(|game| game.id).collect(),
            ..Default::default()
        };
        if let Ok(Ok(reply)) = tokio::time::timeout(Duration::from_secs(6),
            conn.call::<_, CPlayerGetAchievementsProgressResponse>("Player.GetAchievementsProgress#1", &request)).await
        {
            let counts: std::collections::HashMap<_, _> = reply.achievement_progress.iter()
                .map(|item| (item.appid(), (item.unlocked(), item.total()))).collect();
            for game in chunk {
                game.achievement_progress = counts.get(&game.id).copied();
                if game.achievement_progress.is_some_and(|(_, total)| total > 0) {
                    game.achievements = Some(true);
                }
            }
        }
    }
    Ok(games)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "contacts Valve without signing in"]
    async fn steam_provides_a_valid_qr_challenge() {
        let result = connect().await;
        let (conn, _events) = result.ok().expect("could not connect to a trusted Steam CM");
        conn.send(emsg::CLIENT_HELLO, &CMsgClientHello { protocol_version: Some(PROTOCOL_VERSION) })
            .ok().expect("could not start Steam CM session");
        let result = auth::begin_qr(&conn, &auth::Device { name: DEVICE.into() }).await;
        let pending = result.ok().expect("Valve did not return a QR challenge");
        assert!(pending.challenge_url.as_deref().and_then(qr_image).is_some());
    }

    #[test]
    fn accepts_only_steam_guard_challenges() {
        assert!(qr_image("https://s.team/q/1/123456").is_some());
        assert!(qr_image("https://s.team.example/q/1/123456").is_none());
        assert!(qr_image("https://s.team/q/../example").is_none());
        assert!(qr_image("https://s.team/q/1/123456?next=example").is_none());
    }
}
