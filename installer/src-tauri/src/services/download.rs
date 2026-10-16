//! Закачка карт с сайта: entitlement → manifest → файл → проверка → телеметрия.
//!
//! Подписанная ссылка из манифеста живёт недолго, поэтому манифест всегда
//! берём свежим прямо перед скачиванием. Прогресс — событиями
//! `site-download-progress` (`{done_bytes, total_bytes}`). Файл считаем
//! trustworthy только после сверки хеша с манифестом.

use crate::domain::InstallError;
use crate::services::{auth, site};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::io::Write;
use tauri::Emitter;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EntitlementResponse {
    pub map_id: i64,
    pub title: String,
    pub allowed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestVersions {
    pub primary: String,
    #[serde(default)]
    pub supported: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestFile {
    pub url: String,
    pub sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub kind: String,
    pub file_name: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestLoader {
    pub slug: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestIcon {
    pub url: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstallManifest {
    pub map_id: i64,
    pub title: String,
    pub description: Option<String>,
    pub minecraft: ManifestVersions,
    pub file: ManifestFile,
    pub loader: Option<ManifestLoader>,
    pub icon: Option<ManifestIcon>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InstallStartResponse {
    pub idempotency_key: String,
    pub is_first: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DownloadProgress {
    done_bytes: u64,
    total_bytes: Option<u64>,
}

async fn authed_get(path: &str) -> Result<reqwest::Response, InstallError> {
    let token = auth::access_token().await?;
    let client = site::http_client()?;
    let url = format!("{}{path}", site::api_base());
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("site get: {e}")))?;
    // 401-retry (I-2): одиночный 401 при живом refresh пользователь не видит
    // вообще — ни ошибки, ни гостя. Ровно одна попытка: принудительная ротация
    // (`refresh_now` делит один refresh между параллельными командами) + повтор.
    // Провал refresh с `AuthRequired` = сессия мертва; сеть/5xx/429 — транзиент.
    // Повторный 401 = настоящий гость (вызывающий маппит через err_for_status).
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let fresh = auth::refresh_now().await?;
        return client
            .get(&url)
            .bearer_auth(fresh)
            .send()
            .await
            .map_err(|e| InstallError::LaunchFailed(format!("site get retry: {e}")));
    }
    Ok(resp)
}

pub async fn get_entitlement(map_id: i64) -> Result<EntitlementResponse, InstallError> {
    let resp = authed_get(&format!("/maps/{map_id}/entitlement")).await?;
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "проверка доступа").await);
    }
    resp.json()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("entitlement parse: {e}")))
}

pub async fn get_manifest(map_id: i64) -> Result<InstallManifest, InstallError> {
    let resp = authed_get(&format!("/maps/{map_id}/install-manifest")).await?;
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "данные карты").await);
    }
    resp.json()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("manifest parse: {e}")))
}

fn safe_file_name(raw: Option<&str>, map_id: i64, kind: &str) -> String {
    let ext = if kind == "mrpack" { "mrpack" } else { "zip" };
    let base = raw.unwrap_or("").trim();
    let mut clean: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    clean = clean.trim().trim_matches('.').to_string();
    if clean.is_empty() {
        clean = format!("map_{map_id}");
    }
    if !clean.to_lowercase().ends_with(&format!(".{ext}")) {
        clean.push_str(&format!(".{ext}"));
    }
    format!("map_{map_id}_{clean}")
}

/// Скачивание файла карты во временную папку. Возвращает путь к файлу.
pub async fn download_map_file(
    app: &tauri::AppHandle,
    map_id: i64,
) -> Result<String, InstallError> {
    let manifest = get_manifest(map_id).await?;
    let expected = manifest.file.sha256.clone().unwrap_or_default().to_lowercase();
    let total = manifest.file.size_bytes.unwrap_or(0).max(0) as u64;
    let name = safe_file_name(manifest.file.file_name.as_deref(), map_id, &manifest.file.kind);

    let dir = std::env::temp_dir().join("kraimaps");
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&name);
    // Не докачиваем хвосты: ссылка короткоживущая, файл пишем заново.
    let _ = std::fs::remove_file(&dest);

    let client = site::http_client()?;
    let resp = client
        .get(&manifest.file.url)
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("download: {e}")))?;
    if !resp.status().is_success() {
        return Err(InstallError::LaunchFailed(format!(
            "файл недоступен: {}",
            resp.status()
        )));
    }

    let mut file = std::fs::File::create(&dest)?;
    let mut hasher = sha2::Sha256::new();
    let mut done: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| InstallError::LaunchFailed(format!("download: {e}")))?;
        file.write_all(&bytes)?;
        hasher.update(&bytes);
        done += bytes.len() as u64;
        app.emit(
            "site-download-progress",
            DownloadProgress {
                done_bytes: done,
                total_bytes: if total > 0 { Some(total) } else { None },
            },
        )
        .ok();
    }
    file.flush()?;
    drop(file);

    if !expected.is_empty() {
        let actual = hex_of(hasher.finalize());
        if actual != expected {
            let _ = std::fs::remove_file(&dest);
            return Err(InstallError::HashMismatch { expected, actual });
        }
    }
    Ok(dest.to_string_lossy().into_owned())
}

fn hex_of(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Потолок размера иконки: это маленькая картинка, не архив.
const ICON_MAX_BYTES: usize = 8 * 1024 * 1024;

fn icon_temp_path(ext: &str) -> Option<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("kraimaps");
    std::fs::create_dir_all(&dir).ok()?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(dir.join(format!("icon_{}_{}.{}", std::process::id(), nanos, ext)))
}

/// Иконка инстанса — `pack.png`/`icon.png` из архива: корень мира
/// (стандартное место иконки Minecraft — рядом с `level.dat`), затем
/// корень контента и корень архива. Сравнение имён без учёта регистра.
/// Best-effort: не бросает ошибок наружу.
fn extract_pack_icon(
    zip_path: &std::path::Path,
    content_prefix: &str,
    world_prefix: &str,
) -> Option<String> {
    fn norm(prefix: &str) -> String {
        let p = prefix.replace('\\', "/");
        let p = p.trim_matches('/').to_lowercase();
        if p.is_empty() {
            String::new()
        } else {
            format!("{p}/")
        }
    }
    // Порядок: мир → контент → корень. Дубли корней схлопываем.
    let mut roots = vec![norm(world_prefix), norm(content_prefix), String::new()];
    roots.dedup();
    let file = std::fs::File::open(zip_path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    for name in ["pack.png", "icon.png"] {
        for root in &roots {
            let candidate = format!("{root}{name}");
            // Без `?`: отсутствие одного кандидата — не повод бросать
            // остальные (у map_2 нет pack.png, но есть icon.png).
            // Имена декодируем через normalize::decode_name (UTF-8 → CP866):
            // `ZipFile::name()` крейта расходится с world_prefix/content_prefix
            // из analyze_zip на старых архивах без UTF-8 флага (DH.zip: мир
            // «Охотник…» в CP866) — иконка молча не находилась.
            let Some(idx) = (0..zip.len()).find(|&i| {
                zip.by_index(i)
                    .map(|e| {
                        crate::services::normalize::decode_name(e.name_raw())
                            .to_lowercase()
                            == candidate
                    })
                    .unwrap_or(false)
            }) else {
                continue;
            };
            let mut entry = zip.by_index(idx).ok()?;
            if entry.size() as usize > ICON_MAX_BYTES {
                continue;
            }
            let mut bytes = Vec::with_capacity(entry.size().min(1 << 20) as usize);
            use std::io::Read;
            entry.read_to_end(&mut bytes).ok()?;
            if bytes.is_empty() {
                continue;
            }
            let dest = icon_temp_path("png")?;
            std::fs::write(&dest, &bytes).ok()?;
            return Some(dest.to_string_lossy().into_owned());
        }
    }
    None
}

/// Иконка для нового инстанса/профиля — только из архива (никаких
/// привязок к сайту: ручной выбор файла и site-поток ведут себя одинаково).
/// Best-effort: не бросает ошибок наружу.
pub fn prepare_instance_icon(
    zip_path: String,
    content_prefix: String,
    world_prefix: String,
) -> Option<String> {
    extract_pack_icon(
        std::path::Path::new(&zip_path),
        &content_prefix,
        &world_prefix,
    )
}

/// Data URL иконки loose-ресурспака (`pack.png` в корне пака) для превью
/// на экране проверки — по аналогии с иконкой карты. Папка — запись
/// `{source}pack.png` (без учёта регистра, как у карты); zip — `pack.png`
/// в корне внутреннего архива. Best-effort: неуспех — `None`.
pub fn resource_pack_icon_data_url(
    zip_path: &std::path::Path,
    source: &str,
    kind: crate::domain::ResourcePackKind,
) -> Option<String> {
    let bytes = resource_pack_icon_bytes(zip_path, source, kind)?;
    if bytes.is_empty() {
        return None;
    }
    let dest = icon_temp_path("png")?;
    std::fs::write(&dest, &bytes).ok()?;
    crate::services::icon::data_url_for(&dest)
}

/// Байты `pack.png` ресурспака из архива карты.
fn resource_pack_icon_bytes(
    zip_path: &std::path::Path,
    source: &str,
    kind: crate::domain::ResourcePackKind,
) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(zip_path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    match kind {
        crate::domain::ResourcePackKind::Folder => {
            let want = format!("{source}pack.png").to_lowercase();
            let idx = (0..zip.len()).find(|&i| {
                zip.by_index(i)
                    .map(|e| {
                        crate::services::normalize::decode_name(e.name_raw()).to_lowercase() == want
                    })
                    .unwrap_or(false)
            })?;
            let mut entry = zip.by_index(idx).ok()?;
            if entry.size() as usize > ICON_MAX_BYTES {
                return None;
            }
            let mut bytes = Vec::with_capacity(entry.size().min(1 << 20) as usize);
            entry.read_to_end(&mut bytes).ok()?;
            Some(bytes)
        }
        crate::domain::ResourcePackKind::Zip => {
            let idx = (0..zip.len()).find(|&i| {
                zip.by_index(i)
                    .map(|e| crate::services::normalize::decode_name(e.name_raw()) == source)
                    .unwrap_or(false)
            })?;
            let mut packed = Vec::new();
            zip.by_index(idx).ok()?.read_to_end(&mut packed).ok()?;
            let mut inner = zip::ZipArchive::new(std::io::Cursor::new(packed)).ok()?;
            for i in 0..inner.len() {
                let mut entry = inner.by_index(i).ok()?;
                if entry.is_dir() || entry.name().to_lowercase() != "pack.png" {
                    continue;
                }
                if entry.size() as usize > ICON_MAX_BYTES {
                    return None;
                }
                let mut bytes = Vec::with_capacity(entry.size().min(1 << 20) as usize);
                entry.read_to_end(&mut bytes).ok()?;
                return Some(bytes);
            }
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStartArgs {
    pub map_id: i64,
    pub file_sha256: Option<String>,
    pub idempotency_key: Option<String>,
    pub installer_version: Option<String>,
    pub os: Option<String>,
    pub target_kind: Option<String>,
}

pub async fn install_start(args: InstallStartArgs) -> Result<InstallStartResponse, InstallError> {
    let token = auth::access_token().await?;
    let client = site::http_client()?;
    let url = format!("{}/installer/install-start", site::api_base());
    let body = serde_json::json!({
        "map_id": args.map_id,
        "file_sha256": args.file_sha256,
        "idempotency_key": args.idempotency_key,
        "installer_version": args.installer_version,
        "os": args.os,
        "target_kind": args.target_kind,
    });
    let mut resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("install-start: {e}")))?;
    // Тот же одиночный 401-retry, что в `authed_get` (I-2).
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let fresh = auth::refresh_now().await?;
        resp = client
            .post(&url)
            .bearer_auth(fresh)
            .json(&body)
            .send()
            .await
            .map_err(|e| InstallError::LaunchFailed(format!("install-start retry: {e}")))?;
    }
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "начало установки").await);
    }
    resp.json()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("install-start parse: {e}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallCompleteArgs {
    pub idempotency_key: String,
    pub status: String,
    pub error_code: Option<String>,
    pub duration_ms: Option<i64>,
}

pub async fn install_complete(args: InstallCompleteArgs) -> Result<(), InstallError> {
    // Телеметрия не должна ронять UI: без сессии просто пропускаем.
    // Мёртвая сессия — тоже пропуск (телеметрию гостя слать некуда).
    // Транзиент access_token — пропускаем так же: телеметрия не стоит
    // разлогина, её потеря наблюдаема на сервере по отсутствию complete.
    let token = match auth::access_token().await {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let client = site::http_client()?;
    let url = format!("{}/installer/install-complete", site::api_base());
    let body = serde_json::json!({
        "idempotency_key": args.idempotency_key,
        "status": args.status,
        "error_code": args.error_code,
        "duration_ms": args.duration_ms,
    });
    let mut resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("install-complete: {e}")))?;
    // Одиночный 401-retry как везде (I-2); повторный провал — транзиент,
    // телеметрия его глотает молча, сессию не трогаем.
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let fresh = match auth::refresh_now().await {
            Ok(t) => t,
            Err(_) => return Ok(()),
        };
        resp = client
            .post(&url)
            .bearer_auth(fresh)
            .json(&body)
            .send()
            .await
            .map_err(|e| InstallError::LaunchFailed(format!("install-complete retry: {e}")))?;
    }
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "завершение установки").await);
    }
    Ok(())
}

/// Профиль для шапки (`GET /auth/me`).
/// Boot-recovery (I-1): протухший access молча освежается по живому refresh —
/// холодный старт старше 15 минут остаётся авторизованным без ручного входа.
/// Гостя (`Ok`) возвращаем ТОЛЬКО при доказанной смерти сессии (мёртвый refresh).
/// Сеть/5xx/429 — `Err` (транзиент): фронт сохраняет статус и показывает
/// «нет соединения» с ретраем, гостем не объявляет (I-3).
pub async fn auth_profile() -> Result<serde_json::Value, InstallError> {
    // Записи в keyring нет — входить нечему: честный гость без сетевых вызовов.
    // Протухший access `access_token()` освежит сам по живому refresh.
    let token = match auth::access_token().await {
        Ok(t) => t,
        Err(e) if e.is_session_dead() => {
            return Ok(serde_json::json!({ "user": null, "is_authenticated": false }));
        }
        Err(e) => return Err(e),
    };
    let client = site::http_client()?;
    let url = format!("{}/auth/me", site::api_base());
    let mut resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("profile: {e}")))?;
    // 401 на свежий токен — одиночный retry через принудительный refresh (I-2).
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        match auth::refresh_now().await {
            Ok(fresh) => {
                resp = client
                    .get(&url)
                    .bearer_auth(fresh)
                    .send()
                    .await
                    .map_err(|e| InstallError::LaunchFailed(format!("profile retry: {e}")))?;
            }
            Err(e) if e.is_session_dead() => {
                return Ok(serde_json::json!({ "user": null, "is_authenticated": false }));
            }
            Err(e) => return Err(e),
        }
    }
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(serde_json::json!({ "user": null, "is_authenticated": false }));
    }
    if !resp.status().is_success() {
        return Err(site::err_for_status(resp, "профиль").await);
    }
    let raw: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| InstallError::LaunchFailed(format!("profile parse: {e}")))?;
    // Нормализуем к виду {user:{id,username,avatar_url,is_trusted_author},is_authenticated,balance}.
    let user = raw.get("user");
    let username = user
        .and_then(|u| u.get("username"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if username.is_empty() {
        return Ok(serde_json::json!({ "user": null, "is_authenticated": false }));
    }
    Ok(serde_json::json!({
        "user": {
            "id": user.and_then(|u| u.get("id")),
            "username": username,
            "avatar_url": user.and_then(|u| u.get("avatar_url")),
            "is_trusted_author": user.and_then(|u| u.get("is_trusted_author")).unwrap_or(&serde_json::Value::Bool(false)),
        },
        "is_authenticated": true,
        "balance": raw.get("balance"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with(root: &std::path::Path, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = root.join("pack.zip");
        let file = std::fs::File::create(&path).expect("zip");
        let mut zip = zip::ZipWriter::new(file);
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).expect("entry");
            use std::io::Write;
            zip.write_all(data).expect("write");
        }
        zip.finish().expect("finish");
        path
    }

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-icon-{tag}"))
    }

    #[test]
    fn pack_png_found_under_content_prefix() {
        let root = temp_root("prefix");
        let zip_path = zip_with(
            &root,
            &[
                ("Tower of Doom/level.dat", &[1, 2, 3]),
                ("Tower of Doom/Pack.PNG", &[137, 80, 78, 71]),
            ],
        );
        let out = extract_pack_icon(&zip_path, "Tower of Doom/", "Tower of Doom/").expect("icon");
        assert!(out.ends_with(".png"));
        assert!(std::path::Path::new(&out).is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn icon_inside_world_is_found_when_content_prefix_empty() {
        // Регрессия map_2: всё под DeepMine/, content_prefix="" — иконка
        // лежит рядом с level.dat (стандартное место Minecraft).
        let root = temp_root("worldroot");
        let zip_path = zip_with(
            &root,
            &[
                ("DeepMine/level.dat", &[1, 2, 3]),
                ("DeepMine/icon.png", &[137, 80, 78, 71, 1, 2, 3]),
            ],
        );
        let out = extract_pack_icon(&zip_path, "", "DeepMine/").expect("icon");
        assert!(std::path::Path::new(&out).is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pack_png_missing_gives_none() {
        let root = temp_root("none");
        let zip_path = zip_with(&root, &[("World/level.dat", &[1, 2, 3])]);
        assert!(extract_pack_icon(&zip_path, "World/", "World/").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Минимальные PNG-байты: data_url_for смотрит только magic, полный
    /// декод не нужен.
    fn png_bytes() -> Vec<u8> {
        vec![137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3]
    }

    fn inner_pack_with_icon() -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            zip.start_file("pack.mcmeta", zip::write::SimpleFileOptions::default())
                .expect("mcmeta");
            zip.write_all(br#"{"pack":{}}"#).expect("write");
            zip.start_file("pack.png", zip::write::SimpleFileOptions::default())
                .expect("png");
            zip.write_all(&png_bytes()).expect("write");
            zip.finish().expect("finish");
        }
        buf.into_inner()
    }

    #[test]
    fn resource_pack_icon_from_folder() {
        use crate::domain::ResourcePackKind;
        let root = temp_root("rpfolder");
        let zip_path = zip_with(
            &root,
            &[
                ("World/level.dat", &[1, 2, 3]),
                ("RP/pack.mcmeta", b"{}"),
                ("RP/Pack.PNG", &png_bytes()),
            ],
        );
        let url =
            resource_pack_icon_data_url(&zip_path, "RP/", ResourcePackKind::Folder).expect("icon");
        assert!(url.starts_with("data:image/png;base64,"));
        // Нет иконки — None, не ошибка.
        assert!(
            resource_pack_icon_data_url(&zip_path, "Other/", ResourcePackKind::Folder).is_none()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resource_pack_icon_from_inner_zip() {
        use crate::domain::ResourcePackKind;
        let root = temp_root("rpzip");
        let inner = inner_pack_with_icon();
        let zip_path = zip_with(
            &root,
            &[("World/level.dat", &[1, 2, 3]), ("Extra/Cute.zip", &inner)],
        );
        let url = resource_pack_icon_data_url(&zip_path, "Extra/Cute.zip", ResourcePackKind::Zip)
            .expect("icon");
        assert!(url.starts_with("data:image/png;base64,"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// CRC-32 (IEEE) для тестового stored-zip: внешних crc-зависимостей нет.
    fn crc32_ieee(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// Минимальный stored-zip с сырыми (не UTF-8, без флага 0x800) именами —
    /// как старые русские архивы (DH.zip: мир «Охотник» в CP866).
    /// `ZipWriter` так записать не умеет (всегда UTF-8), поэтому вручную:
    /// local headers + central directory + EOCD, метод 0 (без сжатия).
    fn zip_with_raw_names(root: &std::path::Path, entries: &[(&[u8], &[u8])]) -> std::path::PathBuf {
        fn u16le(v: u16, out: &mut Vec<u8>) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        fn u32le(v: u32, out: &mut Vec<u8>) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let path = root.join("pack_raw.zip");
        let mut buf: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in entries {
            let offset = buf.len() as u32;
            let crc = crc32_ieee(data);
            let len = data.len() as u32;
            // Local file header: флаг 0 — бита UTF-8 (0x800) нет.
            u32le(0x0403_4B50, &mut buf);
            u16le(20, &mut buf);
            u16le(0, &mut buf);
            u16le(0, &mut buf); // метод 0 = stored
            u16le(0, &mut buf);
            u16le(0, &mut buf);
            u32le(crc, &mut buf);
            u32le(len, &mut buf);
            u32le(len, &mut buf);
            u16le(name.len() as u16, &mut buf);
            u16le(0, &mut buf);
            buf.extend_from_slice(name);
            buf.extend_from_slice(data);
            // Central directory entry.
            u32le(0x0201_4B50, &mut central);
            u16le(20, &mut central);
            u16le(20, &mut central);
            u16le(0, &mut central);
            u16le(0, &mut central);
            u16le(0, &mut central);
            u16le(0, &mut central);
            u32le(crc, &mut central);
            u32le(len, &mut central);
            u32le(len, &mut central);
            u16le(name.len() as u16, &mut central);
            u16le(0, &mut central);
            u16le(0, &mut central);
            u16le(0, &mut central);
            u16le(0, &mut central);
            u32le(0, &mut central);
            u32le(offset, &mut central);
            central.extend_from_slice(name);
        }
        let central_offset = buf.len() as u32;
        let central_size = central.len() as u32;
        buf.extend_from_slice(&central);
        u32le(0x0605_4B50, &mut buf);
        u16le(0, &mut buf);
        u16le(0, &mut buf);
        u16le(entries.len() as u16, &mut buf);
        u16le(entries.len() as u16, &mut buf);
        u32le(central_size, &mut buf);
        u32le(central_offset, &mut buf);
        u16le(0, &mut buf);
        std::fs::write(&path, &buf).expect("raw zip");
        path
    }

    #[test]
    fn icon_found_when_names_are_cp866_without_utf8_flag() {
        // Регрессия DH.zip: `ZipFile::name()` крейта отдаёт такие имена
        // кракозябрами (CP437), а world_prefix из analyze_zip — честной
        // кириллицей через decode_name. Иконка обязана находиться.
        // «Охотник» в CP866 — живые байты из DH.zip (см. normalize-тест).
        let world_cp866: &[u8] = &[
            0x73, 0x61, 0x76, 0x65, 0x73, 0x2F, // "saves/"
            0x8E, 0xE5, 0xAE, 0xE2, 0xAD, 0xA8, 0xAA, // "Охотник"
            0x2F, // "/"
        ];
        let mut level_name = world_cp866.to_vec();
        level_name.extend_from_slice(b"level.dat");
        let mut icon_name = world_cp866.to_vec();
        icon_name.extend_from_slice(b"icon.png");
        let root = temp_root("cp866");
        let zip_path = zip_with_raw_names(
            &root,
            &[
                (&level_name, &[1, 2, 3]),
                (&icon_name, &[137, 80, 78, 71, 1, 2, 3]),
            ],
        );
        // Префиксы — как их вернул бы analyze_zip (decode_name → кириллица).
        let out = extract_pack_icon(&zip_path, "", "saves/Охотник/").expect("icon");
        assert!(std::path::Path::new(&out).is_file());
        std::fs::remove_dir_all(&root).ok();
    }
}
