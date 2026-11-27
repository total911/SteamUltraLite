use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_VDF_BYTES: u64 = 1024 * 1024;
const FULLY_INSTALLED: u64 = 4;

pub struct Game {
    pub id: u32,
    pub name: String,
    search_name: String,
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

pub fn scan(root: &Path) -> io::Result<Vec<Game>> {
    if !local_path(root) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Expected an absolute local drive path"));
    }
    let root_apps = root.join("steamapps");
    let mut games = HashMap::new();
    scan_folder(&root_apps, &mut games)?;

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
                let _ = scan_folder(&path.join("steamapps"), &mut games);
            }
        }
    }

    let mut games: Vec<_> = games.into_values().collect();
    games.sort_unstable_by(|a, b| a.search_name.cmp(&b.search_name).then(a.id.cmp(&b.id)));
    Ok(games)
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
        games.insert(id, Game { id, name: name.into(), search_name: name.to_lowercase() });
    }
    Ok(())
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

        let games = scan(primary.path()).unwrap();
        assert_eq!(games.iter().map(|game| game.id).collect::<Vec<_>>(), [16, 15]);
        assert_eq!(matches(&games, " ЗВЁЗДНАЯ "), [1]);
        assert_eq!(matches(&games, "16"), [0]);
        assert!(matches(&games, "missing").is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_unc_and_drive_relative_paths_without_io() {
        assert!(!local_path(Path::new(r"\\example.test\games")));
        assert!(!local_path(Path::new(r"C:games")));
    }
}
