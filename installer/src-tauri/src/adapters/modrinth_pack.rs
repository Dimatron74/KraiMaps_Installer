//! Собственный движок `.mrpack` по официальной спецификации Modrinth.
//!
//! Зачем свой: TLauncher, Legacy и официальный лаунчер не умеют импортировать
//! `.mrpack`, а Fabulously Optimized нужен на всех поддерживаемых лаунчерах.
//! Спецификация: Modrinth Help Center, «Modrinth Modpack Format».
//!
//! Индекс — недоверенные данные: путь санитайзится, URL проходит allowlist,
//! SHA-512 обязателен, `server-overrides/` игнорируется, jar не исполняется.

use crate::domain::{CompatFile, InstallError};
use crate::services::http::{self, Expected};
use crate::services::compat_temp::CompatTempFile;
use crate::services::safe_fs::SafeDir;
use crate::services::work_budget::WorkBudget;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::Path;

const MODRINTH_API: &str = "https://api.modrinth.com/v2";
const FO_PROJECT: &str = "fabulously-optimized";

pub const MAX_MRPACK_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_INDEX_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModrinthIndex {
    pub format_version: u32,
    pub game: String,
    pub files: Vec<IndexFile>,
    pub dependencies: IndexDependencies,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexFile {
    pub path: String,
    pub hashes: IndexHashes,
    #[serde(default)]
    pub env: Option<IndexEnv>,
    #[serde(default)]
    pub downloads: Vec<String>,
    pub file_size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexHashes {
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexEnv {
    #[serde(default)]
    pub client: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexDependencies {
    #[serde(default)]
    pub minecraft: Option<String>,
    #[serde(default, rename = "fabric-loader")]
    pub fabric_loader: Option<String>,
}

/// Проверенный вариант Fabulously Optimized под точную версию Minecraft.
#[derive(Debug, Clone)]
pub struct FoVersion {
    pub version_number: String,
    pub file_name: String,
    pub file_url: String,
    pub sha512: String,
    pub size: u64,
}

/// Разобранный и проверенный индекс: готовые файлы для плана.
#[derive(Debug, Clone)]
pub struct ParsedPack {
    pub loader_version: String,
    pub files: Vec<CompatFile>,
    pub total_bytes: u64,
}

/// Относительный путь внутри корня игры. Всё, что может выйти за корень
/// или схлопнуться на Windows (зарезервированные имена, хвостовые точки),
/// отклоняется — это не «строгость ради строгости», а защита данных.
pub fn safe_rel_path(raw: &str) -> Option<String> {
    // Ведущие/хвостовые пробелы Windows схлопывает — такой путь может
    // оказаться не тем файлом, который ожидает пак. Отклоняем целиком.
    if raw.is_empty() || raw != raw.trim() || raw.len() > 512 {
        return None;
    }
    if raw.starts_with('/') || raw.starts_with('\\') {
        return None;
    }
    if raw.chars().any(|c| matches!(c, '\\' | ':' | '\0' | '<' | '>' | '"' | '|' | '?' | '*')) {
        return None;
    }
    if raw.chars().any(char::is_control) {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return None;
    }
    for part in &parts {
        if part.ends_with('.') || part.ends_with(' ') {
            return None;
        }
        let stem = part.split('.').next().unwrap_or_default();
        if is_windows_reserved(stem) {
            return None;
        }
    }
    // Собственный бэкап установщика не должен становиться целью оверрайдов.
    if parts[0].eq_ignore_ascii_case(".kraimaps-backup") {
        return None;
    }
    Some(raw.to_string())
}

fn is_windows_reserved(stem: &str) -> bool {
    let u = stem.to_ascii_uppercase();
    if matches!(u.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    if let Some(number) = u.strip_prefix("COM").or_else(|| u.strip_prefix("LPT")) {
        return matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³");
    }
    false
}

fn is_hex(raw: &str, len: usize) -> bool {
    raw.len() == len && raw.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn path_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// Разбор индекса: проверяет игру, версию MC, наличие `fabric-loader`,
/// пути, URL, SHA-512 и лимиты. Любое расхождение — `E_COMPAT_INVALID`:
/// ставить «полусобранный» пак нельзя.
#[cfg(test)]
pub fn parse_index(raw: &[u8], mc: &str) -> Result<ParsedPack, InstallError> {
    parse_index_with_budget(raw, mc, &WorkBudget::default())
}

pub fn parse_index_with_budget(raw: &[u8], mc: &str, budget: &WorkBudget) -> Result<ParsedPack, InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    let index: ModrinthIndex = serde_json::from_slice(raw)
        .map_err(|e| InstallError::CompatInvalid(format!("modrinth.index.json: {e}")))?;
    if index.game != "minecraft" {
        return Err(InstallError::CompatInvalid(format!(
            "индекс не для Minecraft: {}",
            index.game
        )));
    }
    if index.format_version != 1 {
        return Err(InstallError::CompatInvalid(format!(
            "неподдерживаемый formatVersion: {}",
            index.format_version
        )));
    }
    let index_mc = index
        .dependencies
        .minecraft
        .as_deref()
        .ok_or_else(|| InstallError::CompatInvalid("в индексе нет версии Minecraft".into()))?;
    if index_mc != mc {
        return Err(InstallError::CompatInvalid(format!(
            "сборка для Minecraft {index_mc}, а карта для {mc}"
        )));
    }
    let loader_version = index
        .dependencies
        .fabric_loader
        .clone()
        .ok_or_else(|| InstallError::CompatInvalid("в индексе нет fabric-loader".into()))?;
    if !is_exact_loader_version(&loader_version) {
        return Err(InstallError::CompatInvalid(format!(
            "некорректная версия fabric-loader: {loader_version}"
        )));
    }

    let mut files = Vec::new();
    let mut paths = HashSet::new();
    let mut total_bytes = 0u64;
    for file in &index.files {
        budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
        if file
            .env
            .as_ref()
            .and_then(|e| e.client.as_deref())
            .is_some_and(|v| v.eq_ignore_ascii_case("unsupported"))
        {
            continue;
        }
        let path = safe_rel_path(&file.path).ok_or_else(|| {
            InstallError::CompatInvalid(format!("небезопасный путь в индексе: {}", file.path))
        })?;
        if !paths.insert(path_key(&path)) {
            return Err(InstallError::CompatInvalid(format!("дублирующийся путь в индексе: {path}")));
        }
        if file.file_size == 0 || file.file_size > MAX_FILE_BYTES {
            return Err(InstallError::CompatInvalid(format!(
                "{path}: размер {} вне лимитов",
                file.file_size
            )));
        }
        let sha512 = file
            .hashes
            .sha512
            .as_deref()
            .filter(|h| is_hex(h, 128))
            .ok_or_else(|| {
                InstallError::CompatInvalid(format!("{path}: нет корректного SHA-512"))
            })?;
        let sha1 = file.hashes.sha1.clone();
        if sha1.as_deref().is_some_and(|hash| !is_hex(hash, 40)) {
            return Err(InstallError::CompatInvalid(format!("{path}: некорректный SHA-1")));
        }
        let url = file
            .downloads
            .iter()
            .find(|u| http::pack_url_allowed(u))
            .ok_or_else(|| {
                InstallError::CompatInvalid(format!("{path}: нет доверенного адреса скачивания"))
            })?;
        total_bytes = total_bytes.saturating_add(file.file_size);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(InstallError::CompatInvalid(
                "сборка превышает лимит размера".into(),
            ));
        }
        files.push(CompatFile {
            path,
            url: url.clone(),
            size: file.file_size,
            sha512: sha512.to_ascii_lowercase(),
            sha1,
        });
    }
    if files.is_empty() {
        return Err(InstallError::CompatInvalid(
            "в индексе нет файлов для клиента".into(),
        ));
    }
    Ok(ParsedPack {
        loader_version,
        files,
        total_bytes,
    })
}

fn is_exact_loader_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        && v.chars().any(|c| c.is_ascii_digit())
}

/// Канал API и номер независимы: prerelease-маркер запрещает установку даже в release (§7.4).
pub fn pick_fo_version(doc: &serde_json::Value) -> Option<FoVersion> {
    let arr = doc.as_array()?;
    let mut stable: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|v| v.get("version_type").and_then(|t| t.as_str()) == Some("release"))
        .filter(|v| {
            v.get("version_number").and_then(|number| number.as_str()).is_some_and(|number| {
                let number = number.to_ascii_lowercase();
                !number.is_empty() && !["alpha", "beta", "pre", "rc"]
                    .iter()
                    .any(|marker| number.contains(marker))
            })
        })
        .collect();
    stable.sort_by(|a, b| {
        let da = a.get("date_published").and_then(|d| d.as_str()).unwrap_or("");
        let db = b.get("date_published").and_then(|d| d.as_str()).unwrap_or("");
        db.cmp(da)
    });
    for version in stable {
        let version_number = version
            .get("version_number")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let files = version.get("files")?.as_array()?;
        let primary = files
            .iter()
            .find(|f| {
                f.get("primary").and_then(|p| p.as_bool()).unwrap_or(false)
                    && f.get("filename")
                        .and_then(|n| n.as_str())
                        .is_some_and(|n| n.ends_with(".mrpack"))
            })
            .or_else(|| {
                files.iter().find(|f| {
                    f.get("filename")
                        .and_then(|n| n.as_str())
                        .is_some_and(|n| n.ends_with(".mrpack"))
                })
            })?;
        let file_name = primary.get("filename")?.as_str()?.to_string();
        let file_url = primary.get("url")?.as_str()?.to_string();
        let size = primary.get("size")?.as_u64()?;
        let sha512 = primary
            .get("hashes")?
            .get("sha512")?
            .as_str()?
            .to_ascii_lowercase();
        if !http::pack_url_allowed(&file_url) || !is_hex(&sha512, 128) || size == 0 {
            continue;
        }
        return Some(FoVersion {
            version_number,
            file_name,
            file_url,
            sha512,
            size,
        });
    }
    None
}

/// Свежая версия FO для точной MC. API обязателен: хардкодить версию нельзя
/// (состав пака меняется), а несовпадение ловится разбором индекса.
pub async fn resolve_fo_version(mc: &str, budget: &WorkBudget) -> Result<FoVersion, InstallError> {
    let mut url = url::Url::parse(&format!("{MODRINTH_API}/project/{FO_PROJECT}/version"))
        .map_err(|e| InstallError::CompatResolve(format!("modrinth url: {e}")))?;
    url.query_pairs_mut()
        .append_pair("game_versions", &format!("[\"{mc}\"]"))
        .append_pair("loaders", "[\"fabric\"]");
    let response = budget.network(http::get(url.as_str(), 4 * 1024 * 1024, 3)).await
        .map_err(|error| InstallError::CompatApply(error.to_string()))?
        .map_err(|error| InstallError::CompatResolve(format!("Modrinth API: {}", error.details())))?;
    let mc = mc.to_string();
    budget.blocking(move || {
        let doc: serde_json::Value = serde_json::from_slice(&response.bytes)
            .map_err(|error| InstallError::CompatResolve(format!("Modrinth API: bad json: {error}")))?;
        pick_fo_version(&doc).ok_or_else(|| InstallError::CompatResolve(format!(
            "для Minecraft {mc} нет стабильной сборки Fabulously Optimized"
        )))
    }).await.map_err(|error| InstallError::CompatApply(error.to_string()))?
}

/// Скачивание `.mrpack` с обязательной сверкой SHA-512 из API.
pub async fn download_mrpack(version: &FoVersion, dest: &CompatTempFile, budget: &WorkBudget) -> Result<u64, InstallError> {
    let outcome = http::download_owned(
        &version.file_url,
        dest,
        Expected {
            size: Some(version.size),
            sha512: Some(&version.sha512),
            sha1: None,
            max_bytes: MAX_MRPACK_BYTES,
        },
        &version.file_name,
        None,
        http::DownloadPolicy::default(),
        budget,
    )
    .await?;
    Ok(outcome.bytes)
}

/// Чтение `modrinth.index.json` из корня `.mrpack` (ровно этот путь по спеке).
#[cfg(test)]
pub fn read_index_from_mrpack(mrpack: &[u8]) -> Result<Vec<u8>, InstallError> {
    read_index_with_budget(mrpack, &WorkBudget::default())
}

pub fn read_index_with_budget(mrpack: &[u8], budget: &WorkBudget) -> Result<Vec<u8>, InstallError> {
    let mut zip = zip::ZipArchive::new(budget.reader(Cursor::new(mrpack)))
        .map_err(|e| InstallError::CompatInvalid(format!("mrpack zip: {e}")))?;
    let mut entry = zip
        .by_name("modrinth.index.json")
        .map_err(|_| InstallError::CompatInvalid("в .mrpack нет modrinth.index.json".into()))?;
    if entry.size() > MAX_INDEX_BYTES {
        return Err(InstallError::CompatInvalid("индекс слишком большой".into()));
    }
    let mut raw = Vec::with_capacity(entry.size() as usize);
    let size = entry.size();
    budget.copy(&mut entry.by_ref().take(size + 1), &mut raw)
        .map_err(|e| InstallError::CompatInvalid(format!("index read: {e}")))?;
    if raw.len() as u64 != size {
        return Err(InstallError::CompatInvalid("размер индекса не совпал".into()));
    }
    Ok(raw)
}

/// Пути файлов `overrides/` и `client-overrides/` без извлечения.
/// План хранит их, чтобы после успешного применения можно было отличить
/// файлы слоя в `mods/` от пользовательских при повторе.
#[cfg(test)]
pub fn override_paths(mrpack: &[u8]) -> Result<Vec<String>, InstallError> {
    override_paths_with_budget(mrpack, &WorkBudget::default())
}

pub fn override_paths_with_budget(mrpack: &[u8], budget: &WorkBudget) -> Result<Vec<String>, InstallError> {
    Ok(override_entries(mrpack, 0, budget)?.into_iter().map(|entry| entry.path).collect())
}

pub fn override_size_with_budget(mrpack: &[u8], indexed_bytes: u64, budget: &WorkBudget) -> Result<u64, InstallError> {
    Ok(override_entries(mrpack, indexed_bytes, budget)?.iter().map(|entry| entry.size).sum())
}

pub fn apply_size_with_budget(files: &[CompatFile], mrpack: &[u8], budget: &WorkBudget) -> Result<u64, InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    let indexed_bytes = files.iter().fold(0u64, |sum, file| sum.saturating_add(file.size));
    let mut sizes: HashMap<_, _> = files.iter().map(|file| (path_key(&file.path), file.size)).collect();
    for entry in override_entries(mrpack, indexed_bytes, budget)? {
        budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
        sizes.insert(path_key(&entry.path), entry.size);
    }
    Ok(sizes.values().sum())
}

struct OverrideEntry {
    index: usize,
    path: String,
    size: u64,
}

fn override_entries(mrpack: &[u8], indexed_bytes: u64, budget: &WorkBudget) -> Result<Vec<OverrideEntry>, InstallError> {
    let mut zip = zip::ZipArchive::new(budget.reader(Cursor::new(mrpack)))
        .map_err(|e| InstallError::CompatInvalid(format!("mrpack zip: {e}")))?;
    let mut entries = Vec::new();
    let mut total = indexed_bytes;
    if total > MAX_TOTAL_BYTES {
        return Err(InstallError::CompatInvalid("слой превышает лимит размера".into()));
    }
    for prefix in ["overrides/", "client-overrides/"] {
        let mut paths = HashSet::new();
        for i in 0..zip.len() {
            budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
            let entry = zip
                .by_index(i)
                .map_err(|e| InstallError::CompatInvalid(format!("mrpack entry: {e}")))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name();
            let Some(rel) = name.strip_prefix(prefix) else {
                continue;
            };
            if rel.is_empty() {
                continue;
            }
            if entry.enclosed_name().is_none() {
                return Err(InstallError::CompatInvalid(format!(
                    "небезопасный путь в overrides: {name}"
                )));
            }
            let path = safe_rel_path(rel).ok_or_else(|| {
                InstallError::CompatInvalid(format!("небезопасный путь в overrides: {name}"))
            })?;
            if !paths.insert(path_key(&path)) {
                return Err(InstallError::CompatInvalid(format!("дублирующийся путь в overrides: {name}")));
            }
            if entry.size() > MAX_FILE_BYTES {
                return Err(InstallError::CompatInvalid(format!("{path}: файл overrides больше лимита")));
            }
            total = total.saturating_add(entry.size());
            if total > MAX_TOTAL_BYTES {
                return Err(InstallError::CompatInvalid("слой вместе с overrides превышает лимит размера".into()));
            }
            entries.push(OverrideEntry {
                index: i,
                path,
                size: entry.size(),
            });
        }
    }
    Ok(entries)
}

/// Распаковка `overrides/`, затем `client-overrides/` в staging.
/// `server-overrides/` не нужны клиентской установке. Второй проход
/// перекрывает первый — так спецификация задаёт приоритет client-overrides.
#[cfg(test)]
pub fn extract_overrides(mrpack: &[u8], staging: &Path) -> Result<usize, InstallError> {
    extract_overrides_with_budget(mrpack, staging, &WorkBudget::default())
}

pub fn extract_overrides_with_budget(mrpack: &[u8], staging: &Path, budget: &WorkBudget) -> Result<usize, InstallError> {
    budget.check().map_err(|e| InstallError::CompatApply(e.to_string()))?;
    let entries = override_entries(mrpack, 0, budget)?;
    let target = SafeDir::with_budget(staging, budget.clone())
        .map_err(|e| InstallError::CompatApply(format!("staging: {e}")))?;
    let mut zip = zip::ZipArchive::new(budget.reader(Cursor::new(mrpack)))
        .map_err(|e| InstallError::CompatInvalid(format!("mrpack zip: {e}")))?;
    let mut written = 0usize;
    for item in entries {
        let mut entry = zip.by_index(item.index)
            .map_err(|e| InstallError::CompatInvalid(format!("mrpack entry: {e}")))?;
        target.write_atomic(Path::new(&item.path), |out| {
            let size = budget.copy(&mut entry.by_ref().take(item.size + 1), out)?;
            if size != item.size {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "размер overrides не совпал"));
            }
            Ok(())
        }).map_err(|e| InstallError::CompatApply(format!("{}: {e}", item.path)))?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;
    use std::io::Write;

    #[test]
    fn apply_size_uses_final_client_overrides_instead_of_double_counting_replaced_files() {
        let temp = TempDir::new("mrpack-apply-size");
        let path = temp.join("pack.mrpack");
        crate::test_utils::write_zip(&path, &[
            ("overrides/mods/a.jar", b"ten-bytes!"),
            ("client-overrides/mods/a.jar", b"last"),
            ("overrides/config/a.cfg", b"cfg"),
            ("server-overrides/unused.txt", b"not for client"),
        ]);
        let files = [CompatFile { path: "mods/a.jar".into(), size: 9,
            sha512: "a".repeat(128), sha1: None, url: "https://cdn.modrinth.com/a.jar".into() }];

        let total = apply_size_with_budget(&files, &std::fs::read(&path).expect("pack"), &WorkBudget::default())
            .expect("apply size");

        assert_eq!(total, 7, "only the final staged bytes are copied to the target");
    }

    fn index_json(files: &str, mc: &str, loader: &str) -> String {
        format!(
            r#"{{"formatVersion":1,"game":"minecraft","versionId":"1.0.0","name":"FO",
                "files":[{files}],
                "dependencies":{{"minecraft":"{mc}","fabric-loader":"{loader}"}}}}"#
        )
    }

    fn file_json(path: &str, url: &str, size: u64, env: Option<&str>) -> String {
        let env = env
            .map(|v| format!(r#","env":{{"client":"{v}"}}"#))
            .unwrap_or_default();
        format!(
            r#"{{"path":"{path}","hashes":{{"sha512":"{}","sha1":"{}"}},"downloads":["{url}"],"fileSize":{size}{env}}}"#,
            "a".repeat(128),
            "b".repeat(40)
        )
    }

    #[test]
    fn safe_paths_reject_traversal_and_windows_traps() {
        for ok in [
            "mods/sodium.jar",
            "config/sodium-options.json",
            "resourcepacks/Pack.zip",
            "shaderpacks/a b.zip",
        ] {
            assert_eq!(safe_rel_path(ok).as_deref(), Some(ok), "{ok}");
        }
        for bad in [
            "",
            "../evil",
            "mods/../../evil",
            "/abs/path",
            "\\abs",
            "C:/Windows/x",
            "mods\\x",
            "mods//x",
            "mods/./x",
            "CON",
            "mods/NUL.txt",
            "mods/COM1.jar",
            "mods/trailing.",
            "mods/trailing ",
            ".kraimaps-backup/x",
            "mods/\u{0000}x",
        ] {
            assert_eq!(safe_rel_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn index_rejects_duplicate_destination_paths() {
        let file = file_json("mods/a.jar", "https://cdn.modrinth.com/a.jar", 10, None);
        let raw = index_json(&format!("{file},{file}"), "1.20.1", "0.14.23");

        assert_eq!(parse_index(raw.as_bytes(), "1.20.1").expect_err("duplicate path").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn safe_paths_reject_aliases_of_installer_backups_and_invalid_windows_names() {
        for path in [".KRAIMAPS-BACKUP/a.txt", "config/a?.txt", "config/a*.txt", "config/a|b.txt", "mods/COM¹.jar"] {
            assert!(safe_rel_path(path).is_none(), "{path}");
        }
    }

    #[test]
    fn index_parses_and_skips_client_unsupported() {
        let files = [
            file_json(
                "mods/sodium.jar",
                "https://cdn.modrinth.com/data/a/versions/1/sodium.jar",
                100,
                None,
            ),
            file_json(
                "mods/server-only.jar",
                "https://cdn.modrinth.com/data/b/versions/1/server.jar",
                50,
                Some("unsupported"),
            ),
            file_json(
                "mods/optional.jar",
                "https://cdn.modrinth.com/data/c/versions/1/optional.jar",
                25,
                Some("optional"),
            ),
        ]
        .join(",");
        let raw = index_json(&files, "1.20.1", "0.14.23");
        let parsed = parse_index(raw.as_bytes(), "1.20.1").expect("parse");
        assert_eq!(parsed.loader_version, "0.14.23");
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.total_bytes, 125);
        assert!(parsed.files.iter().all(|f| f.sha512.len() == 128));
    }

    #[test]
    fn index_rejects_malformed_supplied_sha1() {
        for sha1 in ["", "broken", "gggggggggggggggggggggggggggggggggggggggg"] {
            let mut file: serde_json::Value = serde_json::from_str(
                &file_json("mods/a.jar", "https://cdn.modrinth.com/a.jar", 10, None),
            ).expect("file");
            file["hashes"]["sha1"] = serde_json::json!(sha1);
            let raw = index_json(&file.to_string(), "1.20.1", "0.14.23");

            assert_eq!(parse_index(raw.as_bytes(), "1.20.1")
                .expect_err("supplied SHA-1 must be valid").code(), "E_COMPAT_INVALID");
        }
    }

    #[test]
    fn index_preserves_valid_sha1_and_allows_its_absence() {
        for sha1 in [None, Some("A".repeat(40))] {
            let mut file: serde_json::Value = serde_json::from_str(
                &file_json("mods/a.jar", "https://cdn.modrinth.com/a.jar", 10, None),
            ).expect("file");
            if let Some(hash) = &sha1 {
                file["hashes"]["sha1"] = serde_json::json!(hash);
            } else {
                file["hashes"].as_object_mut().expect("hashes").remove("sha1");
            }
            let raw = index_json(&file.to_string(), "1.20.1", "0.14.23");

            let parsed = parse_index(raw.as_bytes(), "1.20.1").expect("valid index");
            assert_eq!(parsed.files[0].sha1, sha1);
        }
    }

    #[test]
    fn index_rejects_other_mc_and_missing_loader() {
        let files = file_json(
            "mods/a.jar",
            "https://cdn.modrinth.com/data/a/versions/1/a.jar",
            10,
            None,
        );
        let raw = index_json(&files, "1.19.2", "0.14.23");
        let err = parse_index(raw.as_bytes(), "1.20.1").expect_err("mc mismatch");
        assert_eq!(err.code(), "E_COMPAT_INVALID");

        let raw = r#"{"formatVersion":1,"game":"minecraft","versionId":"1","files":[],"dependencies":{"minecraft":"1.20.1"}}"#;
        let err = parse_index(raw.as_bytes(), "1.20.1").expect_err("no loader");
        assert_eq!(err.code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn index_rejects_unsafe_path_and_untrusted_url() {
        let unsafe_path = file_json(
            "../evil.jar",
            "https://cdn.modrinth.com/data/a/versions/1/a.jar",
            10,
            None,
        );
        let raw = index_json(&unsafe_path, "1.20.1", "0.14.23");
        assert_eq!(
            parse_index(raw.as_bytes(), "1.20.1")
                .expect_err("unsafe path")
                .code(),
            "E_COMPAT_INVALID"
        );

        let evil_url = file_json("mods/a.jar", "https://evil.example/a.jar", 10, None);
        let raw = index_json(&evil_url, "1.20.1", "0.14.23");
        assert_eq!(
            parse_index(raw.as_bytes(), "1.20.1")
                .expect_err("evil url")
                .code(),
            "E_COMPAT_INVALID"
        );

        let no_hash = r#"{"formatVersion":1,"game":"minecraft","versionId":"1","files":[{"path":"mods/a.jar","hashes":{},"downloads":["https://cdn.modrinth.com/a.jar"],"fileSize":10}],"dependencies":{"minecraft":"1.20.1","fabric-loader":"0.14.23"}}"#;
        assert_eq!(
            parse_index(no_hash.as_bytes(), "1.20.1")
                .expect_err("no sha512")
                .code(),
            "E_COMPAT_INVALID"
        );
    }

    #[test]
    fn fo_version_prefers_newest_release() {
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"[
              {{"id":"old","version_number":"5.0.0","version_type":"release","date_published":"2025-01-01T00:00:00Z",
                "files":[{{"filename":"old.mrpack","url":"https://cdn.modrinth.com/a/old.mrpack","size":10,"primary":true,"hashes":{{"sha512":"{}"}}}}]}},
              {{"id":"new","version_number":"5.4.1","version_type":"release","date_published":"2026-01-01T00:00:00Z",
                "files":[{{"filename":"new.mrpack","url":"https://cdn.modrinth.com/a/new.mrpack","size":20,"primary":true,"hashes":{{"sha512":"{}"}}}}]}},
              {{"id":"beta","version_number":"6.0.0-beta.1","version_type":"beta","date_published":"2026-06-01T00:00:00Z",
                "files":[{{"filename":"beta.mrpack","url":"https://cdn.modrinth.com/a/beta.mrpack","size":30,"primary":true,"hashes":{{"sha512":"{}"}}}}]}}
            ]"#,
            "a".repeat(128),
            "b".repeat(128),
            "c".repeat(128)
        ))
        .expect("json");
        let picked = pick_fo_version(&doc).expect("version");
        assert_eq!(picked.version_number, "5.4.1");
        assert_eq!(picked.file_name, "new.mrpack");
        assert_eq!(picked.size, 20);
    }

    fn fo_version_fixture(number: &str, channel: &str) -> serde_json::Value {
        serde_json::json!({
            "version_number": number,
            "version_type": channel,
            "date_published": "2026-06-01T00:00:00Z",
            "files": [{
                "filename": "FO.mrpack",
                "url": "https://cdn.modrinth.com/FO.mrpack",
                "size": 20,
                "primary": true,
                "hashes": { "sha512": "a".repeat(128) },
            }],
        })
    }

    #[test]
    fn fo_version_does_not_fall_back_to_an_unstable_or_missing_channel() {
        for channel in ["alpha", "beta"] {
            let doc = serde_json::json!([fo_version_fixture("6.0.0", channel)]);
            assert!(pick_fo_version(&doc).is_none(), "{channel} with a plain number is not stable");
        }
        let mut version = fo_version_fixture("6.0.0", "release");
        version.as_object_mut().expect("version").remove("version_type");
        assert!(pick_fo_version(&serde_json::json!([version])).is_none(), "an absent channel is not proof of stability");
    }

    #[test]
    fn fo_release_channel_does_not_override_a_prerelease_number() {
        for number in ["6.0.0-alpha.1", "6.0.0-BETA.1", "6.0.0-pre.1", "6.0.0-rc.1"] {
            let doc = serde_json::json!([fo_version_fixture(number, "release")]);
            assert!(pick_fo_version(&doc).is_none(), "{number} must not be installed as a stable release");
        }
    }

    #[test]
    fn fo_version_rejects_untrusted_file_url() {
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"[{{"id":"x","version_number":"1.0.0","version_type":"release","date_published":"2026-01-01T00:00:00Z",
              "files":[{{"filename":"x.mrpack","url":"https://evil.example/x.mrpack","size":10,"primary":true,"hashes":{{"sha512":"{}"}}}}]}}]"#,
            "a".repeat(128)
        ))
        .expect("json");
        assert!(pick_fo_version(&doc).is_none());
    }

    fn write_mrpack(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        for (name, body) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("entry");
            zip.write_all(body).expect("write");
        }
        zip.finish().expect("finish");
    }

    #[test]
    fn overrides_apply_client_last_and_ignore_server() {
        let dir = TempDir::new("compat-overrides");
        let pack = dir.join("pack.mrpack");
        write_mrpack(
            &pack,
            &[
                ("overrides/config/a.txt", b"base"),
                ("overrides/config/only-base.txt", b"base"),
                ("client-overrides/config/a.txt", b"client"),
                ("server-overrides/config/server.txt", b"server"),
                ("modrinth.index.json", b"{}"),
            ],
        );
        let staging = dir.join("stage");
        std::fs::create_dir_all(&staging).expect("stage");
        let n = extract_overrides(&std::fs::read(&pack).expect("pack"), &staging).expect("extract");
        assert_eq!(n, 3, "два overrides + один client-overrides");
        assert_eq!(
            std::fs::read_to_string(staging.join("config/a.txt")).expect("a"),
            "client",
            "client-overrides перекрывает overrides"
        );
        assert!(staging.join("config/only-base.txt").exists());
        assert!(!staging.join("config/server.txt").exists(), "server ignored");
    }

    #[test]
    fn override_paths_list_client_files_and_ignore_server() {
        let dir = TempDir::new("compat-override-paths");
        let pack = dir.join("pack.mrpack");
        write_mrpack(
            &pack,
            &[
                ("overrides/mods/base.jar", b"base"),
                ("client-overrides/config/client.txt", b"client"),
                ("server-overrides/mods/server.jar", b"server"),
            ],
        );

        let mut paths = override_paths(&std::fs::read(&pack).expect("pack")).expect("paths");
        paths.sort();
        assert_eq!(
            paths,
            vec!["config/client.txt".to_string(), "mods/base.jar".to_string()]
        );
    }

    #[test]
    fn overrides_reject_backslash_paths_before_writing() {
        let dir = TempDir::new("compat-override-backslash");
        let pack = dir.join("pack.mrpack");
        write_mrpack(&pack, &[("overrides/config\\a.txt", b"layer")]);
        let staging = dir.join("stage");
        std::fs::create_dir(&staging).expect("stage");

        assert_eq!(extract_overrides(&std::fs::read(&pack).expect("pack"), &staging).expect_err("backslash").code(), "E_COMPAT_INVALID");
        assert_eq!(std::fs::read_dir(&staging).expect("stage").count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn overrides_reject_case_collisions_within_one_layer() {
        let dir = TempDir::new("compat-override-duplicate");
        let pack = dir.join("pack.mrpack");
        write_mrpack(&pack, &[
            ("overrides/config/A.txt", b"first"),
            ("overrides/config/a.txt", b"second"),
        ]);
        let staging = dir.join("stage");
        std::fs::create_dir(&staging).expect("stage");

        assert_eq!(extract_overrides(&std::fs::read(&pack).expect("pack"), &staging).expect_err("case collision").code(), "E_COMPAT_INVALID");
        assert_eq!(std::fs::read_dir(&staging).expect("stage").count(), 0);
    }

    #[test]
    fn overrides_refuse_directory_links_in_staging() {
        let outside = TempDir::new("compat-override-link-outside");
        let dir = TempDir::new("compat-override-link");
        let pack = dir.join("pack.mrpack");
        write_mrpack(&pack, &[("overrides/config/a.txt", b"layer")]);
        let staging = dir.join("stage");
        std::fs::create_dir(&staging).expect("stage");
        crate::test_utils::link_directory(&outside, &staging.join("config"));

        let result = extract_overrides(&std::fs::read(&pack).expect("pack"), &staging);

        assert!(result.is_err(), "staging directory link must be refused");
        assert!(!outside.join("a.txt").exists());
    }

    #[test]
    fn overrides_reject_zip_slip() {
        let dir = TempDir::new("compat-slip");
        let pack = dir.join("pack.mrpack");
        write_mrpack(&pack, &[("overrides/../../evil.txt", b"x")]);
        let staging = dir.join("stage");
        std::fs::create_dir_all(&staging).expect("stage");
        let err = extract_overrides(&std::fs::read(&pack).expect("pack"), &staging).expect_err("slip");
        assert_eq!(err.code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn missing_index_is_invalid() {
        let dir = TempDir::new("compat-noindex");
        let pack = dir.join("pack.mrpack");
        write_mrpack(&pack, &[("overrides/a.txt", b"x")]);
        assert_eq!(
            read_index_from_mrpack(&std::fs::read(&pack).expect("pack")).expect_err("no index").code(),
            "E_COMPAT_INVALID"
        );
    }

    /// Живой резолв FO: сеть, поэтому только по явному запуску.
    #[tokio::test]
    #[ignore = "живой тест сети (Modrinth API)"]
    async fn live_fo_resolves_for_1_20_1() {
        let budget = WorkBudget::within(std::time::Duration::from_secs(20 * 60));
        let v = resolve_fo_version("1.20.1", &budget).await.expect("FO 1.20.1");
        assert!(v.file_name.ends_with(".mrpack"));
        assert!(v.size > 0);
        let dir = TempDir::new("compat-live");
        let root = crate::test_utils::compat_root(&dir);
        let dest = root.file(Path::new("pack.mrpack")).expect("pack destination");
        download_mrpack(&v, &dest, &budget).await.expect("download");
        let raw = read_index_from_mrpack(&std::fs::read(&dest).expect("pack")).expect("index");
        let parsed = parse_index(&raw, "1.20.1").expect("parse");
        assert!(!parsed.files.is_empty());
        assert!(!parsed.loader_version.is_empty());
    }
}
