use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::time::Duration;

use net::{Connection, emsg};
use proto::*;
use sha1::{Digest, Sha1};

use crate::library::Game;

type Packages = [(u32, u64)];
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
const LIBRARY_TYPES: &[&str] = &["game", "demo", "application", "tool", "music", "video", "dlc"];

async fn product_info(
    conn: &Connection,
    req: &CMsgClientPicsProductInfoRequest,
) -> net::Result<Vec<CMsgClientPicsProductInfoResponse>> {
    conn.request_many(emsg::CLIENT_PICS_PRODUCT_INFO_REQUEST, req, |packet| {
        !packet.body::<CMsgClientPicsProductInfoResponse>().is_ok_and(|reply| reply.response_pending())
    })
    .await?
    .iter()
    .map(decode_reply)
    .collect()
}

fn decode_reply<M: prost::Message + Default>(packet: &net::Packet) -> net::Result<M> {
    packet.check_if_reported()?.body()
}

pub async fn resolve(conn: &Connection, packages: &Packages) -> net::Result<Vec<Game>> {
    let mut ids = BTreeSet::new();
    for batch in packages.chunks(200) {
        let request = CMsgClientPicsProductInfoRequest {
            packages: batch.iter().map(|&(id, token)| c_msg_client_pics_product_info_request::PackageInfo {
                packageid: Some(id),
                access_token: Some(token),
            }).collect(),
            ..Default::default()
        };
        for response in product_info(conn, &request).await? {
            for package in response.packages {
                if !batch.iter().any(|&(id, _)| id == package.packageid()) {
                    continue;
                }
                if let Some(bytes) = package.buffer.as_deref().and_then(|b| b.get(4..)) {
                    ids.extend(package_app_ids(bytes));
                }
            }
        }
    }

    let http = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let ids: Vec<u32> = ids.into_iter().collect();
    let mut games = Vec::new();
    for batch in ids.chunks(180) {
        let token_reply = conn.request(
            emsg::CLIENT_PICS_ACCESS_TOKEN_REQUEST,
            &CMsgClientPicsAccessTokenRequest { appids: batch.to_vec(), packageids: Vec::new() },
        ).await?;
        let tokens: CMsgClientPicsAccessTokenResponse = decode_reply(&token_reply)?;
        let tokens: HashMap<_, _> = tokens.app_access_tokens.iter().map(|item| (item.appid(), item.access_token())).collect();
        let request = CMsgClientPicsProductInfoRequest {
            apps: batch.iter().map(|&id| c_msg_client_pics_product_info_request::AppInfo {
                appid: Some(id),
                access_token: tokens.get(&id).copied(),
                only_public_obsolete: None,
            }).collect(),
            ..Default::default()
        };
        for response in product_info(conn, &request).await? {
            for app in response.apps {
                let id = app.appid();
                if batch.binary_search(&id).is_err() {
                    continue;
                }
                let metadata = match app.buffer.as_deref().filter(|bytes| !bytes.is_empty()) {
                    Some(bytes) => std::str::from_utf8(bytes).ok().map(str::to_owned),
                    None => match (response.http_host.as_deref(), app.sha.as_deref()) {
                        (Some(host), Some(sha)) => fetch_metadata(&http, host, id, sha).await,
                        _ => None,
                    },
                };
                if let Some(game) = metadata.as_deref().and_then(|text| parse_game(id, text)) {
                    games.push(game);
                }
            }
        }
    }
    crate::library::sort(&mut games);
    Ok(games)
}

fn package_app_ids(bytes: &[u8]) -> Vec<u32> {
    let Ok(doc) = vdf::binary::parse(bytes) else { return Vec::new() };
    doc.iter()
        .filter_map(|(_, package)| package.get("appids")?.as_obj())
        .flat_map(vdf::Obj::iter)
        .filter_map(|(_, appid)| appid.as_str()?.parse::<u32>().ok())
        .filter(|id| *id != 0)
        .collect()
}

fn parse_game(id: u32, metadata: &str) -> Option<Game> {
    let root = vdf::parse(metadata.trim_end_matches('\0')).ok()?;
    let info = root.obj("appinfo")?;
    let common = info.obj("common")?;
    let kind = common.str("type")?.to_ascii_lowercase();
    if !LIBRARY_TYPES.contains(&kind.as_str()) {
        return None;
    }
    let name = common.str("name")?.trim();
    if name.is_empty() { return None; }
    let mut game = Game::new(id, name);
    let associations = common.obj("associations");
    let names = |kind: &str| -> Option<String> {
        let values: Vec<_> = associations.into_iter().flat_map(vdf::Obj::iter)
            .filter_map(|(_, value)| value.as_obj())
            .filter(|value| value.str("type").is_some_and(|value| value.eq_ignore_ascii_case(kind)))
            .filter_map(|value| value.str("name"))
            .filter(|value| !value.trim().is_empty())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    };
    game.developer = names("developer").or_else(|| common.str("developer").map(str::to_owned));
    game.publisher = names("publisher").or_else(|| common.str("publisher").map(str::to_owned));
    if let Some(categories) = common.obj("category") {
        game.achievements = Some(categories.get("category_22").is_some());
        game.cards = Some(categories.get("category_29").is_some());
    }
    game.content = Some(model::OwnedApp {
        app_id: id,
        name: name.to_owned(),
        kind,
        install_dir: info.obj("config").and_then(|config| config.str("installdir")).unwrap_or_default().to_owned(),
        depots: parse_depots(info),
        branches: info.path(&["depots", "branches"]).into_iter().flat_map(vdf::Obj::iter)
            .filter_map(|(name, value)| {
                let branch = value.as_obj()?;
                Some(model::Branch {
                    name: name.to_owned(),
                    build_id: branch.u64("buildid").unwrap_or(0),
                    ..Default::default()
                })
            }).collect(),
        ..Default::default()
    });
    Some(game)
}

pub fn parse_depots(info: &vdf::Obj) -> Vec<model::DepotInfo> {
    let Some(depots) = info.obj("depots") else { return Vec::new() };
    depots.iter().filter_map(|(id, value)| {
        let id = id.parse().ok()?;
        let item = value.as_obj()?;
        let config = item.obj("config");
        let option = |name| config.and_then(|config| config.str(name)).unwrap_or_default().to_owned();
        let manifests = item.obj("manifests").into_iter().flat_map(vdf::Obj::iter)
            .filter_map(|(branch, value)| {
                let (gid, size) = match value {
                    vdf::Value::Obj(obj) => (obj.u64("gid").unwrap_or(0), obj.u64("size").unwrap_or(0)),
                    vdf::Value::Str(text) => (text.parse().unwrap_or(0), 0),
                };
                (gid != 0).then(|| (branch.to_owned(), gid, size))
            }).collect();
        Some(model::DepotInfo {
            id,
            os: option("oslist"),
            arch: option("osarch"),
            language: option("language"),
            low_violence: option("lowviolence") == "1",
            dlc_app: item.u64("dlcappid").map(|id| id as u32),
            shared_install: item.str("sharedinstall") == Some("1") || item.u64("depotfromapp").is_some(),
            from_app: item.u64("depotfromapp").map(|id| id as u32),
            manifests,
        })
    }).collect()
}

pub async fn raw_app_info(conn: &Connection, id: u32) -> Option<vdf::Obj> {
    let tokens: CMsgClientPicsAccessTokenResponse = decode_reply(&conn.request(
        emsg::CLIENT_PICS_ACCESS_TOKEN_REQUEST,
        &CMsgClientPicsAccessTokenRequest { appids: vec![id], packageids: Vec::new() },
    ).await.ok()?).ok()?;
    let access_token = tokens.app_access_tokens.iter().find(|token| token.appid() == id).map(|token| token.access_token());
    let request = CMsgClientPicsProductInfoRequest {
        apps: vec![c_msg_client_pics_product_info_request::AppInfo {
            appid: Some(id), access_token, only_public_obsolete: None,
        }],
        ..Default::default()
    };
    let http = reqwest::Client::builder().https_only(true).no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15)).build().ok()?;
    for response in product_info(conn, &request).await.ok()? {
        for app in response.apps.into_iter().filter(|app| app.appid() == id) {
            let text = match app.buffer.as_deref().filter(|bytes| !bytes.is_empty()) {
                Some(bytes) => std::str::from_utf8(bytes).ok()?.to_owned(),
                None => fetch_metadata(&http, response.http_host.as_deref()?, id, app.sha.as_deref()?).await?,
            };
            return vdf::parse(text.trim_end_matches('\0')).ok()?.obj("appinfo").cloned();
        }
    }
    None
}

fn content_host(host: &str) -> bool {
    let host = host.strip_suffix(":443").unwrap_or(host);
    host.len() < 254
        && [".steamstatic.com", ".steamcontent.com", ".steamserver.net"].iter().any(|suffix| host.ends_with(suffix))
        && host.split('.').all(|label| {
            !label.is_empty() && !label.starts_with('-') && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

async fn fetch_metadata(http: &reqwest::Client, host: &str, id: u32, sha: &[u8]) -> Option<String> {
    if sha.len() != 20 || !content_host(host) {
        return None;
    }
    let hash: String = sha.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut response = http.get(format!("https://{host}/appinfo/{id}/sha/{hash}.txt.gz"))
        .send().await.ok()?.error_for_status().ok()?;
    let mut compressed = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if compressed.len().saturating_add(chunk.len()) > MAX_METADATA_BYTES {
            return None;
        }
        compressed.extend_from_slice(&chunk);
    }
    decode_metadata(compressed, sha)
}

fn decode_metadata(compressed: Vec<u8>, sha: &[u8]) -> Option<String> {
    let mut decoded = Vec::new();
    if compressed.starts_with(&[0x1f, 0x8b]) {
        flate2::read::GzDecoder::new(compressed.as_slice())
            .take((MAX_METADATA_BYTES + 1) as u64)
            .read_to_end(&mut decoded)
            .ok()?;
    } else {
        decoded.extend_from_slice(&compressed);
    }
    let body = decoded.strip_suffix(b"\0").unwrap_or(&decoded);
    if decoded.len() > MAX_METADATA_BYTES || [body, &decoded, &compressed]
        .iter().all(|bytes| Sha1::digest(bytes).as_slice() != sha)
    {
        return None;
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_catalog_sources_and_content() {
        assert!(content_host("clientconfig.akamai.steamstatic.com:443"));
        assert!(!content_host("steamstatic.com.example.org"));
        assert!(!content_host("evil.org@clientconfig.akamai.steamstatic.com"));
        assert!(!content_host("127.0.0.1"));
        let text = br#""appinfo" { "common" { "type" "Game" "name" "Example" } }"#.to_vec();
        let sha = Sha1::digest(&text);
        assert_eq!(decode_metadata(text.clone(), &sha).as_deref().and_then(|text| parse_game(42, text)).unwrap().name, "Example");
        assert!(decode_metadata(text, &[0; 20]).is_none());
        let detailed = r#""appinfo" { "common" { "type" "Game" "name" "Example"
            "associations" { "0" { "type" "developer" "name" "Studio" }
                             "1" { "type" "publisher" "name" "Publisher" } }
            "category" { "category_22" "1" "category_29" "1" } } }"#;
        let game = parse_game(42, detailed).unwrap();
        assert_eq!(game.developer.as_deref(), Some("Studio"));
        assert_eq!(game.publisher.as_deref(), Some("Publisher"));
        assert_eq!(game.achievements, Some(true));
        assert_eq!(game.cards, Some(true));
        let depot = r#""appinfo" { "common" { "type" "Game" "name" "Example" }
            "config" { "installdir" "Example" }
            "depots" { "123" { "config" { "oslist" "windows" }
                "manifests" { "public" { "gid" "456" "size" "1024" } } } } }"#;
        let content = parse_game(42, depot).unwrap().content.unwrap();
        assert_eq!(content.install_dir, "Example");
        assert_eq!(content.depots[0].manifest("public"), Some((456, 1024)));
    }
}
