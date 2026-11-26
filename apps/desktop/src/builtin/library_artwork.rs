use super::*;
use model::art;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ArtKind {
    /// The small square icon in the game list.
    Icon,
    /// The library cover, 600 × 900.
    Capsule,
    /// The background at the top of the game's page, 3840 × 1240.
    Hero,
    /// The game's logo over the background.
    Logo,
    /// The wide header, 920 × 430 (list view, recent games).
    Header,
}

impl ArtKind {
    pub const ALL: [ArtKind; 5] = [ArtKind::Capsule, ArtKind::Hero, ArtKind::Logo, ArtKind::Header, ArtKind::Icon];

    /// Its file name in our folder.
    fn ours(self) -> &'static str {
        match self {
            ArtKind::Icon => "icon",
            ArtKind::Capsule => "capsule",
            ArtKind::Hero => "hero",
            ArtKind::Logo => "logo",
            ArtKind::Header => "header",
        }
    }

    /// Its file name in Steam's `config/grid`, without the extension.
    fn steam(self, id: AppId) -> String {
        match self {
            ArtKind::Icon => format!("{id}_icon"),
            ArtKind::Capsule => format!("{id}p"),
            ArtKind::Hero => format!("{id}_hero"),
            ArtKind::Logo => format!("{id}_logo"),
            ArtKind::Header => format!("{id}"),
        }
    }
}

const PICTURE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "ico"];

/// Where Steam keeps this account's non-Steam games and custom art
/// (`<steam>/userdata/<account>/config`), set once it's known.
static STEAM_CONFIG: RwLock<Option<PathBuf>> = RwLock::new(None);
/// Art found per game and kind (looking costs a few disk checks).
static FOUND: RwLock<Option<HashMap<(AppId, ArtKind), Option<String>>>> = RwLock::new(None);

/// Where our custom art goes. The demo uses a folder of its own, so it
/// never touches the user's.
pub fn art_root() -> PathBuf {
    if crate::app::demo() {
        std::env::temp_dir().join("steaminrust-demo").join("custom-art")
    } else {
        client::store::config_dir().join("custom-art")
    }
}

/// Tell the art lookup where Steam's files are. Returns true when it changed.
pub fn set_steam_config(dir: Option<PathBuf>) -> bool {
    let mut cur = STEAM_CONFIG.write().unwrap();
    if *cur == dir {
        return false;
    }
    *cur = dir;
    forget_found();
    true
}

pub fn steam_config() -> Option<PathBuf> {
    STEAM_CONFIG.read().unwrap().clone()
}

pub(super) fn forget_found() {
    *FOUND.write().unwrap() = None;
}

/// Custom art's image addresses: `sir-art://<path>`, read by [`ArtLoader`]
/// (the app has no general `file://` loader, on purpose).
pub const ART_SCHEME: &str = "sir-art://";

fn file_uri(p: &Path) -> String {
    format!("{ART_SCHEME}{}", p.display())
}

/// Reads custom art from disk: only pictures inside our art folder or
/// Steam's `config/grid`, never any other file.
pub struct ArtLoader;

impl ArtLoader {
    pub fn install(ctx: &egui::Context) {
        ctx.add_bytes_loader(std::sync::Arc::new(ArtLoader));
    }
}

/// Whether a path is a picture inside one of the art folders.
fn allowed(path: &Path) -> bool {
    let ext_ok = path.extension().is_some_and(|e| PICTURE_EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()));
    let Ok(real) = path.canonicalize() else { return false };
    let roots = [Some(art_root()), steam_config().map(|c| c.join("grid"))];
    ext_ok && roots.into_iter().flatten().filter_map(|r| r.canonicalize().ok()).any(|r| real.starts_with(r))
}

impl egui::load::BytesLoader for ArtLoader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(ArtLoader)
    }

    fn load(&self, _ctx: &egui::Context, uri: &str) -> egui::load::BytesLoadResult {
        use egui::load::{BytesPoll, LoadError};
        let Some(path) = uri.strip_prefix(ART_SCHEME) else { return Err(LoadError::NotSupported) };
        let path = Path::new(path);
        if !allowed(path) {
            return Err(LoadError::Loading(format!("{}not an artwork file", ui::widgets::GONE)));
        }
        match std::fs::read(path) {
            Ok(bytes) => Ok(BytesPoll::Ready { size: None, bytes: egui::load::Bytes::Shared(bytes.into()), mime: None }),
            Err(e) => Err(LoadError::Loading(format!("{}{e}", ui::widgets::GONE))),
        }
    }

    fn forget(&self, _uri: &str) {}

    fn forget_all(&self) {}

    fn byte_size(&self) -> usize {
        0
    }
}

/// The newest picture in `dir` named `<stem>.<ext>` or `<stem>-<n>.<ext>`.
fn find(dir: &Path, stem: &str) -> Option<PathBuf> {
    let read = std::fs::read_dir(dir).ok()?;
    read.flatten()
        .map(|e| e.path())
        .filter(|p| {
            let (Some(s), Some(e)) = (p.file_stem().and_then(|s| s.to_str()), p.extension().and_then(|e| e.to_str())) else { return false };
            PICTURE_EXTS.contains(&e.to_ascii_lowercase().as_str()) && (s == stem || s.strip_prefix(stem).and_then(|r| r.strip_prefix('-')).is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())))
        })
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// Where a custom picture comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArtSource {
    Ours,
    Steam,
}

/// The custom picture for a game, as an image address, and where it's from.
pub fn custom_art_from(id: AppId, kind: ArtKind) -> Option<(String, ArtSource)> {
    let dir = art_root().join(id.to_string());
    if let Some(p) = find(&dir, kind.ours()) {
        return Some((file_uri(&p), ArtSource::Ours));
    }
    // "Cleared" hides Steam's picture too.
    if dir.join(format!("{}.none", kind.ours())).exists() {
        return None;
    }
    let grid = steam_config()?.join("grid");
    find(&grid, &kind.steam(id)).map(|p| (file_uri(&p), ArtSource::Steam))
}

/// The custom picture for a game, if any (remembered until art changes).
pub fn custom_art(id: AppId, kind: ArtKind) -> Option<String> {
    if let Some(found) = FOUND.read().unwrap().as_ref().and_then(|m| m.get(&(id, kind))) {
        return found.clone();
    }
    let found = custom_art_from(id, kind).map(|(uri, _)| uri);
    FOUND.write().unwrap().get_or_insert_with(Default::default).insert((id, kind), found.clone());
    found
}

/// Use a picture from disk: copied into our folder (a new name each time,
/// so the old one isn't shown from memory).
pub fn set_art(id: AppId, kind: ArtKind, src: &Path) -> Result<(), String> {
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).filter(|e| PICTURE_EXTS.contains(&e.as_str())).ok_or("Pick a PNG, JPEG, WebP or ICO picture")?;
    let dir = art_root().join(id.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    remove_ours(&dir, kind);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    std::fs::copy(src, dir.join(format!("{}-{stamp}.{ext}", kind.ours()))).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dir.join(format!("{}.none", kind.ours())));
    forget_found();
    Ok(())
}

/// Stop using a custom picture (Steam's own, or none, shows again). Art
/// from Steam's folder is hidden rather than deleted.
pub fn clear_art(id: AppId, kind: ArtKind) {
    let dir = art_root().join(id.to_string());
    let had_ours = find(&dir, kind.ours()).is_some();
    remove_ours(&dir, kind);
    if !had_ours && custom_art_from(id, kind).is_some_and(|(_, s)| s == ArtSource::Steam) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("{}.none", kind.ours())), b"");
    }
    forget_found();
}

fn remove_ours(dir: &Path, kind: ArtKind) {
    while let Some(p) = find(dir, kind.ours()) {
        if std::fs::remove_file(&p).is_err() {
            break;
        }
    }
}

/// Where the logo sits on the background, in Steam's format
/// (`config/grid/<id>.json`: `{"nVersion":1,"logoPosition":{…}}`).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct LogoPosition {
    /// "BottomLeft", "UpperLeft", "CenterCenter", "UpperCenter", "BottomCenter".
    #[serde(rename = "pinnedPosition")]
    pub pin: Pin,
    /// Size, as a share of the background's width and height (0–100).
    #[serde(rename = "nWidthPct")]
    pub width_pct: f32,
    #[serde(rename = "nHeightPct")]
    pub height_pct: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Pin {
    BottomLeft,
    UpperLeft,
    CenterCenter,
    UpperCenter,
    BottomCenter,
}

impl Pin {
    pub const ALL: [Pin; 5] = [Pin::BottomLeft, Pin::UpperLeft, Pin::CenterCenter, Pin::UpperCenter, Pin::BottomCenter];
}

impl Default for LogoPosition {
    fn default() -> Self {
        Self { pin: Pin::BottomLeft, width_pct: 36.0, height_pct: 60.0 }
    }
}

#[derive(Serialize, Deserialize)]
struct LogoFile {
    #[serde(rename = "nVersion")]
    version: u32,
    #[serde(rename = "logoPosition")]
    position: LogoPosition,
}

/// The logo's position: ours, else Steam's, else none (the page's default).
pub fn logo_position(id: AppId) -> Option<LogoPosition> {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<LogoFile>(&s).ok()).map(|f| f.position);
    read(art_root().join(id.to_string()).join("logo.json")).or_else(|| read(steam_config()?.join("grid").join(format!("{id}.json"))))
}

pub fn set_logo_position(id: AppId, pos: Option<LogoPosition>) {
    let dir = art_root().join(id.to_string());
    let path = dir.join("logo.json");
    match pos {
        Some(position) => {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(path, serde_json::to_string(&LogoFile { version: 1, position }).unwrap_or_default());
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Where the logo goes on a background `hero`.
pub fn logo_rect(hero: egui::Rect, pos: LogoPosition, margin: f32) -> egui::Rect {
    let area = hero.shrink(margin);
    let size = egui::vec2(hero.width() * pos.width_pct.clamp(5.0, 100.0) / 100.0, hero.height() * pos.height_pct.clamp(5.0, 100.0) / 100.0).min(area.size());
    let (x, y) = match pos.pin {
        Pin::BottomLeft => (area.left(), area.bottom() - size.y),
        Pin::UpperLeft => (area.left(), area.top()),
        Pin::CenterCenter => (area.center().x - size.x / 2.0, area.center().y - size.y / 2.0),
        Pin::UpperCenter => (area.center().x - size.x / 2.0, area.top()),
        Pin::BottomCenter => (area.center().x - size.x / 2.0, area.bottom() - size.y),
    };
    egui::Rect::from_min_size(egui::pos2(x, y), size)
}

/// How a picture of this kind sits in its box on the logo's pin.
pub fn pin_align(pin: Pin) -> egui::Align2 {
    match pin {
        Pin::BottomLeft => egui::Align2::LEFT_BOTTOM,
        Pin::UpperLeft => egui::Align2::LEFT_TOP,
        Pin::CenterCenter => egui::Align2::CENTER_CENTER,
        Pin::UpperCenter => egui::Align2::CENTER_TOP,
        Pin::BottomCenter => egui::Align2::CENTER_BOTTOM,
    }
}

// ---------- a game's pictures, custom first ----------

/// The cover (capsule): custom, else Steam's; none for a non-Steam game.
pub fn capsule(id: AppId) -> String {
    custom_art(id, ArtKind::Capsule).unwrap_or_else(|| if is_shortcut(id) { String::new() } else { art::capsule(id) })
}
pub fn header(id: AppId) -> String {
    custom_art(id, ArtKind::Header).unwrap_or_else(|| if is_shortcut(id) { String::new() } else { art::header(id) })
}
pub fn hero(id: AppId) -> String {
    custom_art(id, ArtKind::Hero).unwrap_or_else(|| if is_shortcut(id) { String::new() } else { art::hero(id) })
}
pub fn logo(id: AppId) -> String {
    custom_art(id, ArtKind::Logo).unwrap_or_else(|| if is_shortcut(id) { String::new() } else { art::logo(id) })
}

/// The list icon: custom, else the game's own (Steam's icon hash, or a
/// non-Steam game's picture file).
pub fn icon(id: AppId, steam_hash: Option<&str>, shortcut_icon: Option<&str>) -> Option<String> {
    custom_art(id, ArtKind::Icon)
        .or_else(|| steam_hash.map(|h| art::icon(id, h)))
        .or_else(|| {
            let p = Path::new(shortcut_icon?.trim().trim_matches('"'));
            let ext = p.extension()?.to_string_lossy().to_lowercase();
            (matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "ico") && allowed(p)).then(|| file_uri(p))
        })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_positions_round_trip_in_steams_format() {
        let json = r#"{"nVersion":1,"logoPosition":{"pinnedPosition":"UpperCenter","nWidthPct":50.0,"nHeightPct":40.0}}"#;
        let f: LogoFile = serde_json::from_str(json).unwrap();
        assert_eq!(f.position.pin, Pin::UpperCenter);
        assert_eq!(f.position.width_pct, 50.0);
        let back = serde_json::to_string(&f).unwrap();
        assert!(back.contains("\"pinnedPosition\":\"UpperCenter\"") && back.contains("\"nVersion\":1"));
        let hero = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 300.0));
        let r = logo_rect(hero, f.position, 20.0);
        assert_eq!(r.center().x, 500.0);
        assert_eq!(r.top(), 20.0);
    }

    #[test]
    fn picks_our_newest_picture_and_steams_names() {
        let dir = std::env::temp_dir().join(format!("sir-art-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hero-1.png"), b"x").unwrap();
        std::fs::write(dir.join("hero-2.txt"), b"x").unwrap();
        std::fs::write(dir.join("heroes.png"), b"x").unwrap();
        assert_eq!(find(&dir, "hero").and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())), Some("hero-1.png".into()));
        assert_eq!(ArtKind::Capsule.steam(123), "123p");
        assert_eq!(ArtKind::Header.steam(123), "123");
        assert_eq!(ArtKind::Hero.steam(123), "123_hero");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
