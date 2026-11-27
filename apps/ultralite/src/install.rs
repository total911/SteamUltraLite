use std::io::Read;
use std::path::{Path, PathBuf};

use model::OwnedApp;

use crate::{download, library};
use crate::library::Game;

pub fn request(app: &OwnedApp, library: &Path, russian: bool) -> Result<download::InstallRequest, String> {
    if app.app_id == 0 || !library::valid_root(library) {
        return Err("Choose a local Steam library folder".into());
    }
    let steamapps = library.join("steamapps");
    for ancestor in library.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        depot::install::ensure_real_dir(ancestor).map_err(|_| "Unsafe library folder")?;
    }
    depot::install::ensure_real_dir(&steamapps).map_err(|_| "Unsafe steamapps folder")?;
    let common = steamapps.join("common");
    depot::install::ensure_real_dir(&common).map_err(|_| "Unsafe common folder")?;
    let folder = if app.install_dir.is_empty() { &app.name } else { &app.install_dir };
    depot::install::validate_folder_name(folder).map_err(|_| "Unsafe game folder name")?;
    let dir = depot::install::resolve_inside(&common, folder).map_err(|_| "Unsafe game folder path")?;
    let manifest = steamapps.join(format!("appmanifest_{}.acf", app.app_id));
    match std::fs::symlink_metadata(&manifest) {
        Ok(_) => return Err("Game is already installed in that library".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not inspect game manifest".into()),
    }
    let cache = library.join("depotcache");
    depot::install::ensure_real_dir(&cache).map_err(|_| "Unsafe depot cache")?;
    if cache.exists() {
        for entry in std::fs::read_dir(&cache).map_err(|_| "Could not inspect depot cache")? {
            let entry = entry.map_err(|_| "Could not inspect depot cache")?;
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(|_| "Could not inspect depot cache")?;
            if local::is_symlink_or_reparse(&metadata) {
                return Err("Unsafe depot cache entry".into());
            }
        }
    }
    if dir.exists() && !(dir.join(".sirnew").is_file() && dir.join(".sirresume").is_file()) {
        return Err("Game folder already contains files".into());
    }
    Ok(download::InstallRequest {
        app_id: app.app_id,
        library: PathBuf::from(library),
        steam_root: PathBuf::from(library),
        branch: "public".into(),
        language: if russian { "russian" } else { "english" }.into(),
        skip_dlc: Vec::new(),
        add_dlc: Vec::new(),
    })
}

pub fn remove(game: &Game) -> Result<(), String> {
    let root = game.library_root.as_ref().ok_or("Unknown library root")?;
    let dir = game.install_dir.as_ref().ok_or("Unknown game folder")?;
    if !library::valid_root(root) {
        return Err("Library folder is not available".into());
    }
    for ancestor in root.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        depot::install::ensure_real_dir(ancestor).map_err(|_| "Unsafe library folder")?;
    }
    let manifest = root.join("steamapps").join(format!("appmanifest_{}.acf", game.id));
    let metadata = std::fs::symlink_metadata(&manifest).map_err(|_| "Game manifest is unavailable")?;
    if local::is_symlink_or_reparse(&metadata) {
        return Err("Unsafe game manifest".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&manifest).map_err(|_| "Game manifest is unavailable")?
        .take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "Could not read game manifest")?;
    if bytes.len() > 1024 * 1024 {
        return Err("Game manifest is too large".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "Invalid game manifest")?;
    let document = vdf::parse(&text).map_err(|_| "Invalid game manifest")?;
    let state = document.obj("AppState").ok_or("Invalid game manifest")?;
    let folder = dir.file_name().and_then(|value| value.to_str()).ok_or("Invalid game folder")?;
    if state.u64("appid") != Some(u64::from(game.id)) || state.str("installdir") != Some(folder) {
        return Err("Game manifest changed; scan again".into());
    }
    let common = root.join("steamapps").join("common");
    local::validate_common_child(&common, dir)?;
    let installed = model::InstalledGame {
        app_id: game.id,
        name: game.name.clone(),
        install_dir: dir.clone(),
        library: root.clone(),
        size_on_disk: state.u64("SizeOnDisk").unwrap_or(0),
        last_updated: None,
        last_update_bytes: None,
        last_played: None,
        playtime_minutes: game.playtime_minutes,
        state: model::InstallState::Installed,
    };
    local::manage::uninstall_with(&installed, &local::manage::Progress::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_selected_game_and_keeps_sibling() {
        let root = tempfile::tempdir().unwrap();
        let steamapps = root.path().join("steamapps");
        let common = steamapps.join("common");
        std::fs::create_dir_all(common.join("Chosen")).unwrap();
        std::fs::create_dir_all(common.join("Neighbour")).unwrap();
        std::fs::write(common.join("Chosen/game.exe"), "game").unwrap();
        std::fs::write(common.join("Neighbour/save.dat"), "keep").unwrap();
        let state = vdf::Obj::default()
            .with("appid", "42").with("name", "Chosen")
            .with("StateFlags", "4").with("installdir", "Chosen");
        std::fs::write(steamapps.join("appmanifest_42.acf"),
            vdf::write(&vdf::Obj::default().with_obj("AppState", state))).unwrap();
        let mut game = Game::new(42, "Chosen");
        game.install_dir = Some(common.join("Chosen"));
        game.library_root = Some(root.path().to_path_buf());
        remove(&game).unwrap();
        assert!(!common.join("Chosen").exists());
        assert!(!steamapps.join("appmanifest_42.acf").exists());
        assert_eq!(std::fs::read_to_string(common.join("Neighbour/save.dat")).unwrap(), "keep");
    }

    #[test]
    fn refuses_existing_game_files_and_manifest() {
        let root = tempfile::tempdir().unwrap();
        let steamapps = root.path().join("steamapps");
        std::fs::create_dir_all(steamapps.join("common")).unwrap();
        let app = OwnedApp { app_id: 42, name: "Test".into(), install_dir: "Test".into(), ..Default::default() };
        assert!(request(&app, root.path(), false).is_ok());
        std::fs::create_dir(steamapps.join("common/Test")).unwrap();
        assert!(request(&app, root.path(), false).is_err());
        std::fs::write(steamapps.join("common/Test/.sirnew"), "").unwrap();
        std::fs::write(steamapps.join("common/Test/.sirresume"), "").unwrap();
        assert!(request(&app, root.path(), false).is_ok());
        std::fs::remove_file(steamapps.join("common/Test/.sirnew")).unwrap();
        std::fs::remove_file(steamapps.join("common/Test/.sirresume")).unwrap();
        std::fs::remove_dir(steamapps.join("common/Test")).unwrap();
        std::fs::write(steamapps.join("appmanifest_42.acf"), "existing").unwrap();
        assert!(request(&app, root.path(), false).is_err());
    }
}

