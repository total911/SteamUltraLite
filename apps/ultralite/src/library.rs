use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_VDF_BYTES: u64 = 1024 * 1024;
const FULLY_INSTALLED: u64 = 4;

#[derive(Clone)]
pub struct Game {
    pub id: u32,
    pub name: String,
    pub developer: Option<String>,
    pub publisher: Option<String>,
    pub achievements: Option<bool>,
    pub cards: Option<bool>,
    pub install_dir: Option<PathBuf>,
    pub library_root: Option<PathBuf>,
    pub playtime_minutes: Option<u64>,
    pub achievement_progress: Option<(u32, u32)>,
    pub content: Option<model::OwnedApp>,
    search_name: String,
}

impl Game {
    pub fn new(id: u32, name: &str) -> Self {
        Self { id, name: name.into(), developer: None, publisher: None, achievements: None, cards: None, install_dir: None, library_root: None, playtime_minutes: None, achievement_progress: None, content: None, search_name: name.to_lowercase() }
    }
}

pub fn valid_root(path: &Path) -> bool {
    local_path(path) && path.join("steamapps").is_dir()
}

pub fn default_root() -> Option<PathBuf> {
    ["STEAM_ROOT", "ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(|key| {
            let path = PathBuf::from(std::env::var_os(key)?);
            Some(if key == "STEAM_ROOT" { path } else { path.join("Steam") })
        })
        .find(|path| local_path(path) && path.join("steamapps").is_dir())
}

#[cfg(windows)]
fn local_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    use windows_sys::Win32::{
        Storage::FileSystem::GetDriveTypeW,
        System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
    };

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let (Prefix::Disk(drive) | Prefix::VerbatimDisk(drive)) = prefix.kind() else {
        return false;
    };
    if !path.is_absolute() {
        return false;
    }
    let root = [u16::from(drive), b':' as u16, b'\\' as u16, 0];
    // GetDriveTypeW reads the type of this NUL-terminated drive root.
    matches!(unsafe { GetDriveTypeW(root.as_ptr()) }, DRIVE_FIXED | DRIVE_REMOVABLE)
}

#[cfg(not(windows))]
fn local_path(path: &Path) -> bool {
    path.is_absolute()
}

fn read_vdf(path: &Path) -> io::Result<vdf::Obj> {
    let mut text = String::new();
    File::open(path)?.take(MAX_VDF_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_VDF_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "VDF exceeds 1 MiB"));
    }
    vdf::parse(text.trim_start_matches('\u{feff}')).map_err(io::Error::other)
}

pub fn scan_roots(roots: &[PathBuf]) -> io::Result<Vec<Game>> {
    let mut games = HashMap::new();
    let mut first_error = None;
    let mut read_any = false;
    for root in roots {
        if !local_path(root) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Expected an absolute local drive path"));
        }
        match scan_root(root, &mut games) {
            Ok(()) => read_any = true,
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    if !read_any && let Some(error) = first_error {
        return Err(error);
    }
    let mut games: Vec<_> = games.into_values().collect();
    sort(&mut games);
    Ok(games)
}

fn scan_root(root: &Path, games: &mut HashMap<u32, Game>) -> io::Result<()> {
    if !local_path(root) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Expected an absolute local drive path"));
    }
    let root_apps = root.join("steamapps");
    scan_folder(&root_apps, games)?;

    if let Ok(document) = read_vdf(&root_apps.join("libraryfolders.vdf"))
        && let Some(folders) = document.obj("libraryfolders")
    {
        for (key, value) in folders.iter() {
            if key.parse::<u32>().is_err() {
                continue;
            }
            let path = value.as_str().or_else(|| value.get("path")?.as_str());
            if let Some(path) = path.map(Path::new)
                && local_path(path)
            {
                // A disconnected secondary drive should not hide other games.
                let _ = scan_folder(&path.join("steamapps"), games);
            }
        }
    }

    Ok(())
}

fn scan_folder(steamapps: &Path, games: &mut HashMap<u32, Game>) -> io::Result<()> {
    for entry in fs::read_dir(steamapps)?.flatten() {
        let path = entry.path();
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("appmanifest_")?.strip_suffix(".acf"))
            .and_then(|id| id.parse::<u32>().ok())
            .filter(|id| *id > 0)
        else {
            continue;
        };
        if games.contains_key(&id) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(doc) = read_vdf(&path) else { continue };
        let Some(app) = doc.obj("AppState") else { continue };
        if app.u64("appid") != Some(u64::from(id))
            || app.u64("StateFlags").unwrap_or(0) & FULLY_INSTALLED == 0
        {
            continue;
        }
        let Some(name) = app.str("name").map(str::trim).filter(|name| !name.is_empty()) else {
            continue;
        };
        let mut game = Game::new(id, name);
        if let Some(folder) = app.str("installdir").filter(|folder| valid_folder(folder)) {
            let dir = steamapps.join("common").join(folder);
            if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) {
                game.install_dir = Some(dir);
            }
        }
        game.library_root = steamapps.parent().map(Path::to_path_buf);
        games.insert(id, game);
    }
    Ok(())
}

fn valid_folder(folder: &str) -> bool {
    !folder.is_empty() && folder != "." && folder != ".."
        && !folder.ends_with([' ', '.'])
        && !folder.chars().any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c))
}

pub fn sort(games: &mut [Game]) {
    games.sort_unstable_by(|a, b| a.search_name.cmp(&b.search_name).then(a.id.cmp(&b.id)));
}

pub fn matches(games: &[Game], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let app_id = query.parse::<u32>().ok();
    games
        .iter()
        .enumerate()
        .filter(|(_, game)| Some(game.id) == app_id || game.search_name.contains(&query))
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_multiple_libraries_and_finds_names_and_app_ids() {
        let primary = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for path in [primary.path(), second.path()] {
            fs::create_dir(path.join("steamapps")).unwrap();
        }
        let index = vdf::Obj::default().with_obj(
            "libraryfolders",
            vdf::Obj::default().with_obj(
                "1",
                vdf::Obj::default().with("path", second.path().to_string_lossy()),
            ),
        );
        fs::write(primary.path().join("steamapps/libraryfolders.vdf"), vdf::write(&index)).unwrap();
        for (root, filename, id, name, flags) in [
            (primary.path(), 15, 15, "Звёздная игра", "4"),
            (second.path(), 15, 15, "Duplicate", "4"),
            (second.path(), 16, 16, "Alpha", "4"),
            (second.path(), 17, 18, "Wrong AppID", "4"),
            (second.path(), 19, 19, "Incomplete", "2"),
        ] {
            let doc = vdf::Obj::default().with_obj(
                "AppState",
                vdf::Obj::default().with("appid", id.to_string()).with("name", name).with("StateFlags", flags),
            );
            fs::write(root.join(format!("steamapps/appmanifest_{filename}.acf")), vdf::write(&doc)).unwrap();
        }

        let malformed = second.path().join("steamapps/appmanifest_20.acf");
        fs::write(malformed, b"\"AppState\" {").unwrap();

        let games = scan_roots(&[primary.path().to_path_buf()]).unwrap();
        assert_eq!(games.iter().map(|game| game.id).collect::<Vec<_>>(), [16, 15]);
        assert_eq!(matches(&games, " ЗВЁЗДНАЯ "), [1]);
        assert_eq!(matches(&games, "16"), [0]);
        assert!(matches(&games, "missing").is_empty());
        let independent = tempfile::tempdir().unwrap();
        fs::create_dir(independent.path().join("steamapps")).unwrap();
        let doc = vdf::Obj::default().with_obj(
            "AppState",
            vdf::Obj::default().with("appid", "30").with("name", "Other disk").with("StateFlags", "4"),
        );
        fs::write(independent.path().join("steamapps/appmanifest_30.acf"), vdf::write(&doc)).unwrap();
        let roots = [primary.path().to_path_buf(), independent.path().to_path_buf()];
        let all = scan_roots(&roots).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(matches(&all, "Other disk").len(), 1);
        let absent = independent.path().join("disconnected");
        let surviving = scan_roots(&[absent, primary.path().to_path_buf()]).unwrap();
        assert_eq!(surviving.len(), 2);
    }

    #[test]
    fn rejects_unsafe_install_folders() {
        for folder in ["..", "../other", r"..\other", "C:other", "name.", "name ", "CON?"] {
            assert!(!valid_folder(folder), "{folder}");
        }
        assert!(valid_folder("Game folder"));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_unc_and_drive_relative_paths_without_io() {
        assert!(!local_path(Path::new(r"\\example.test\games")));
        assert!(!local_path(Path::new(r"C:games")));
    }
}
