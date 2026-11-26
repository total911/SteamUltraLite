use client::Event;
use model::OwnedApp;

pub fn fake_library_path() -> Option<std::path::PathBuf> {
    if !crate::app::demo() {
        return None;
    }
    std::env::args().find_map(|arg| arg.strip_prefix("--demo-library=").map(std::path::PathBuf::from))
}

pub fn events(local: &local::LocalState) -> Vec<Event> {
    let mut apps: Vec<OwnedApp> = local.games.iter().map(|game| app(game.app_id, &game.name)).collect();
    for (id, name) in [(440, "Team Fortress 2"), (570, "Dota 2"), (620, "Portal 2"), (730, "Counter-Strike 2"), (1145360, "Hades"), (413150, "Stardew Valley"), (292030, "The Witcher 3: Wild Hunt")] {
        if !apps.iter().any(|game| game.app_id == id) {
            apps.push(app(id, name));
        }
    }
    vec![
        Event::LoggedIn { steam_id: 76561197960265729, account: "demo".into() },
        Event::Apps(apps),
        Event::Download(model::DownloadStatus {
            app_id: 1145360,
            phase: model::DownloadPhase::Downloading,
            downloaded: 46,
            download_total: 100,
            disk_done: 46,
            disk_total: 100,
            ..Default::default()
        }),
    ]
}

fn app(app_id: model::AppId, name: &str) -> OwnedApp {
    OwnedApp { app_id, name: name.into(), kind: "game".into(), ..Default::default() }
}
