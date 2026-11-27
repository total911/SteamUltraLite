use std::path::PathBuf;

use crate::library::Game;

fn direct_executable(id: u32) -> Option<&'static str> {
    match id {
        3048420 => Some("SetYourselfonFire.exe"),
        757170 => Some("2D Kolobok.exe"),
        _ => None,
    }
}

pub fn direct(game: &Game) -> Result<Option<PathBuf>, String> {
    let Some(name) = direct_executable(game.id) else { return Ok(None) };
    let dir = game.install_dir.as_ref().ok_or("Game folder is unavailable")?;
    for ancestor in dir.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        depot::install::ensure_real_dir(ancestor).map_err(|_| "Unsafe game folder")?;
    }
    let exe = dir.join(name);
    let metadata = std::fs::symlink_metadata(&exe).map_err(|_| "Game executable is unavailable")?;
    if !metadata.is_file() || local::is_symlink_or_reparse(&metadata) {
        return Err("Unsafe game executable".into());
    }
    Ok(Some(exe))
}

pub fn start(game: &Game) -> Result<(), String> {
    if let Some(exe) = direct(game)? {
        let dir = game.install_dir.as_ref().ok_or("Game folder is unavailable")?;
        std::process::Command::new(exe).current_dir(dir).spawn()
            .map_err(|error| format!("Could not start the game: {error}"))?;
    } else {
        open::that_detached(format!("steam://run/{}", game.id))
            .map_err(|error| format!("Could not open Steam: {error}"))?;
    }
    Ok(())
}

pub fn uses_steam(id: u32) -> bool {
    direct_executable(id).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_launch_is_limited_to_verified_games_and_real_files() {
        let folder = tempfile::tempdir().unwrap();
        let mut game = Game::new(3048420, "Test");
        game.install_dir = Some(folder.path().to_path_buf());
        assert!(direct(&game).is_err());
        std::fs::write(folder.path().join("SetYourselfonFire.exe"), b"test").unwrap();
        assert_eq!(direct(&game).unwrap(), Some(folder.path().join("SetYourselfonFire.exe")));
        game.id = 757170;
        assert!(direct(&game).is_err());
        game.id = 610670;
        assert!(direct(&game).unwrap().is_none());
    }
}
