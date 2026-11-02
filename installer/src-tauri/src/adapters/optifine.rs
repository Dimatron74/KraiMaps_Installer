//! Получение официального OptiFine jar с `optifine.net` и подготовка
//! Forge-версии со страницы загрузок.
//!
//! У OptiFine нет API и CLI: только HTML-страница, временный токен и файл
//! за ним. Поэтому здесь живут три парсера (список сборок, токен, Forge)
//! и жёсткая валидация: HTML с кодом 200 обязан отвергаться, а jar —
//! проверяться по содержимому, а не по расширению.
//!
//! Токен и полный URL с токеном не логируются и не покидают модуль.

use crate::domain::InstallError;
use crate::services::http::{self, DownloadOutcome, DownloadPolicy, Expected};
use crate::services::compat_temp::CompatTempFile;
use crate::services::safe_fs::SafeDir;
use crate::services::work_budget::WorkBudget;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use scraper::{Html, Selector};
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const BASE: &str = "https://optifine.net";
const DOWNLOADS_URL: &str = "https://optifine.net/downloads";
const MAVEN_METADATA: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";

const PAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;
const TOKEN_PAGE_MAX_BYTES: u64 = 2 * 1024 * 1024;
const METADATA_MAX_BYTES: u64 = 16 * 1024 * 1024;

pub const MIN_JAR_BYTES: u64 = 1024 * 1024;
pub const MAX_JAR_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MARKER_BYTES: u64 = 1024 * 1024;

/// Символы, которые нельзя оставлять в значении query без кодирования.
const QUERY_VALUE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'&')
    .add(b'+')
    .add(b'?')
    .add(b'%')
    .add(b'<')
    .add(b'>');

/// Совместимая версия Forge, указанная на странице OptiFine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeStatus {
    /// Полная версия (`47.2.18`) — использовать как есть.
    Full(String),
    /// Только build number (`#2847`) — резолвится через Maven.
    Build(String),
    /// `N/A` или строка не найдена: Forge-путь для этой сборки недоступен.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptifineRelease {
    pub file_name: String,
    pub forge: ForgeStatus,
}

/// Разрешённая сборка: точное имя файла и полная версия Forge. Ничего не
/// скачивает — это нужно `prepare_compat`, чтобы показать план до деплоя.
#[derive(Debug, Clone)]
pub struct OptifineReleasePlan {
    pub release: OptifineRelease,
    pub forge_version: String,
}

/// Стабильная сборка OptiFine для точной MC. Preview не рассматриваем:
/// для поддерживаемого диапазона (≤ 1.16.4) стабильные есть всегда.
pub fn parse_downloads(page: &str, mc: &str) -> Option<OptifineRelease> {
    let document = Html::parse_document(page);
    let rows = Selector::parse("tr.downloadLine").expect("static row selector");
    let links = Selector::parse("td.colMirror a[href]").expect("static link selector");
    let forge_cells = Selector::parse("td.colForge").expect("static Forge selector");
    let prefix = format!("OptiFine_{mc}_");
    for row in document.select(&rows) {
        let file_name = row
            .select(&links)
            .filter_map(|link| link.value().attr("href"))
            .flat_map(adloadx_names)
            .find(|name| name.starts_with(&prefix) && name.ends_with(".jar"));
        let Some(file_name) = file_name else {
            continue;
        };
        // Download и Mirror дублируют jar; совместимость задаёт только
        // colForge той же строки, а не текст после следующей ссылки.
        let forge = row
            .select(&forge_cells)
            .next()
            .and_then(|cell| forge_token(&cell.text().collect::<Vec<_>>().join(" ")))
            .unwrap_or(ForgeStatus::Unavailable);
        return Some(OptifineRelease { file_name, forge });
    }
    None
}

/// Временный токен из ответа `adloadx`. Ищем именно ссылку на нужный файл:
/// на странице могут быть и другие ссылки, и чужая ссылка означала бы
/// скачивание не того jar.
pub fn parse_token(page: &str, file_name: &str) -> Option<String> {
    let raw = html_unescape(page);
    let mut rest = raw.as_str();
    while let Some(pos) = rest.find("downloadx?f=") {
        rest = &rest[pos + "downloadx?f=".len()..];
        let end = rest
            .find(|c: char| matches!(c, '&' | '"' | '\'' | '<' | '>' | ' ' | '\t' | '\r' | '\n'))
            .unwrap_or(rest.len());
        let linked = percent_decode_str(&rest[..end])
            .decode_utf8_lossy()
            .into_owned();
        if linked == file_name {
            if let Some(token) = rest[end..].strip_prefix("&x=") {
                let token: String = token
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                if !token.is_empty() {
                    return Some(token);
                }
            }
        }
        rest = &rest[end..];
    }
    None
}

/// Полная версия Forge для `loader_version` инстанса. Build number требует
/// сети: страница OptiFine для старых MC знает только `#2847`.
pub async fn resolve_forge_version(
    mc: &str,
    status: &ForgeStatus,
    budget: &WorkBudget,
) -> Result<String, InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    match status {
        ForgeStatus::Full(v) => Ok(v.clone()),
        ForgeStatus::Build(build) => {
            let xml = budget.network(http::get_text(MAVEN_METADATA, METADATA_MAX_BYTES, 3))
                .await.map_err(|error| InstallError::CompatApply(error.to_string()))?
                .map_err(|e| {
                    InstallError::CompatResolve(format!("Forge Maven: {}", e.details()))
                })?;
            let (mc, build) = (mc.to_string(), build.clone());
            budget.blocking(move || pick_forge_build(&xml, &mc, &build).ok_or_else(|| {
                InstallError::CompatUnavailable(format!("Forge {mc}-{build} не найден в Maven"))
            })).await.map_err(|error| InstallError::CompatApply(error.to_string()))?
        }
        ForgeStatus::Unavailable => Err(InstallError::CompatUnavailable(format!(
            "OptiFine для Minecraft {mc} не указывает совместимую версию Forge"
        ))),
    }
}

/// Резолв страницы загрузок без скачивания файла.
pub async fn resolve_release(mc: &str, budget: &WorkBudget) -> Result<OptifineReleasePlan, InstallError> {
    let page = budget.network(http::get_text(DOWNLOADS_URL, PAGE_MAX_BYTES, 3)).await
        .map_err(|error| InstallError::CompatApply(error.to_string()))??;
    let owned_mc = mc.to_string();
    let release = budget.blocking(move || parse_downloads(&page, &owned_mc).ok_or_else(|| {
        InstallError::CompatUnavailable(format!("стабильной сборки OptiFine для Minecraft {owned_mc} нет"))
    })).await.map_err(|error| InstallError::CompatApply(error.to_string()))??;
    let forge_version = resolve_forge_version(mc, &release.forge, budget).await?;
    Ok(OptifineReleasePlan {
        release,
        forge_version,
    })
}

/// Скачивание уже выбранного файла. Имя приходит из плана, поэтому страница
/// повторно не разбирается: между подготовкой и деплоем не должно появиться
/// «другого» jar.
pub async fn download_jar_named(
    file_name: &str,
    dest: &CompatTempFile,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    parent: &WorkBudget,
) -> Result<String, InstallError> {
    let budget = parent.limited_to(DownloadPolicy::default().file_timeout);
    let _cancel = budget.cancel_on_drop();
    budget.network(download_named_with_budget(file_name, dest, progress, &budget)).await
        .map_err(|error| InstallError::CompatStalled(format!("{file_name}: {error}")))?
}

async fn download_named_with_budget(
    file_name: &str,
    dest: &CompatTempFile,
    progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    budget: &WorkBudget,
) -> Result<String, InstallError> {
    if !is_safe_file_name(file_name) {
        return Err(InstallError::CompatInvalid(format!(
            "недопустимое имя файла OptiFine: {file_name}"
        )));
    }
    let mut token = fetch_token(file_name, budget).await?;
    let mut last_error: Option<InstallError> = None;
    let high_water = AtomicU64::new(0);
    let monotonic = |done: u64, total: u64| {
        let previous = high_water.fetch_max(done, Ordering::Relaxed);
        if done > previous {
            if let Some(callback) = progress {
                callback(done, total);
            }
        }
    };
    for attempt in 0..2 {
        if attempt > 0 {
            token = fetch_token(file_name, budget).await?;
        }
        let url = format!(
            "{BASE}/downloadx?f={}&x={token}",
            utf8_percent_encode(file_name, QUERY_VALUE)
        );
        let outcome = http::download_owned(
            &url,
            dest,
            Expected {
                size: None,
                sha512: None,
                sha1: None,
                max_bytes: MAX_JAR_BYTES,
            },
            file_name,
            Some(&monotonic),
            DownloadPolicy::default(),
            budget,
        )
        .await?;
        let (worker_dest, worker_name, worker_budget) = (dest.clone(), file_name.to_string(), budget.clone());
        let validated = budget.blocking(move || {
            validate_download_with_budget(&outcome, &worker_name, &worker_dest, &worker_budget)
        }).await.map_err(|error| InstallError::CompatApply(error.to_string()))?;
        match validated {
            Ok(sha256) => return Ok(sha256),
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        InstallError::CompatDownload(format!("{file_name}: не удалось получить jar"))
    }))
}

async fn fetch_token(file_name: &str, budget: &WorkBudget) -> Result<String, InstallError> {
    let url = format!(
        "{BASE}/adloadx?f={}",
        utf8_percent_encode(file_name, QUERY_VALUE)
    );
    let page = budget.network(http::get_text(&url, TOKEN_PAGE_MAX_BYTES, 3)).await
        .map_err(|error| InstallError::CompatStalled(format!("{file_name}: {error}")))??;
    let file_name = file_name.to_string();
    budget.blocking(move || parse_token(&page, &file_name).ok_or_else(|| {
        InstallError::CompatDownload(format!(
            "{file_name}: страница не отдала download-токен"
        ))
    })).await.map_err(|error| InstallError::CompatApply(error.to_string()))?
}

#[cfg(test)]
fn validate_download(
    outcome: &DownloadOutcome,
    expected_name: &str,
    dest: &Path,
) -> Result<String, InstallError> {
    validate_download_with_budget(outcome, expected_name, dest, &WorkBudget::default())
}

fn validate_download_with_budget(
    outcome: &DownloadOutcome,
    expected_name: &str,
    dest: &Path,
    budget: &WorkBudget,
) -> Result<String, InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    let content_type = outcome.content_type.as_str();
    if content_type.contains("text/html") {
        return Err(InstallError::CompatInvalid(format!(
            "{expected_name}: сервер вернул HTML вместо jar"
        )));
    }
    let archive_ok = content_type.contains("java-archive")
        || content_type.contains("zip")
        || content_type.contains("octet-stream");
    if !archive_ok {
        return Err(InstallError::CompatInvalid(format!(
            "{expected_name}: неожиданный Content-Type: {content_type}"
        )));
    }
    let host = url::Url::parse(&outcome.final_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    if host.as_deref() != Some("optifine.net") {
        return Err(InstallError::CompatInvalid(format!(
            "{expected_name}: файл пришёл не с optifine.net"
        )));
    }
    if let Some(disposition) = &outcome.content_disposition {
        let decoded = percent_decode_str(disposition)
            .decode_utf8_lossy()
            .to_ascii_lowercase();
        if !decoded.contains(&expected_name.to_ascii_lowercase()) {
            return Err(InstallError::CompatInvalid(format!(
                "{expected_name}: имя в Content-Disposition не совпало"
            )));
        }
    }
    validate_jar_with_budget(dest, expected_name, budget)
}

/// Проверка «это действительно установщик OptiFine, пригодный для Forge».
/// Trust anchor — HTTPS + содержимое ZIP: официальных контрольных сумм нет,
/// поэтому SHA-256 считаем сами для «Подробнее» и логов.
#[cfg(test)]
pub fn validate_jar(path: &Path, expected_name: &str) -> Result<String, InstallError> {
    validate_jar_with_budget(path, expected_name, &WorkBudget::default())
}

pub fn validate_jar_with_budget(path: &Path, expected_name: &str, budget: &WorkBudget) -> Result<String, InstallError> {
    let parent = path.parent().ok_or_else(|| InstallError::CompatInvalid("у jar нет каталога".into()))?;
    let bytes = SafeDir::with_budget(parent, budget.clone())
        .and_then(|root| root.read_bounded(Path::new(path.file_name().unwrap_or_default()), MAX_JAR_BYTES))
        .map_err(|e| InstallError::CompatInvalid(format!("{expected_name}: {e}")))?;
    if (bytes.len() as u64) < MIN_JAR_BYTES {
        return Err(InstallError::CompatInvalid(format!(
            "{expected_name}: размер {} байт вне допустимого диапазона",
            bytes.len()
        )));
    }
    let mut zip = zip::ZipArchive::new(budget.reader(Cursor::new(&bytes)))
        .map_err(|e| InstallError::CompatInvalid(format!("{expected_name}: не zip/jar: {e}")))?;

    let manifest = read_entry(&mut zip, "META-INF/MANIFEST.MF", budget)?
        .ok_or_else(|| InstallError::CompatInvalid(format!("{expected_name}: нет MANIFEST.MF")))?;
    if read_entry(&mut zip, "optifine/Installer.class", budget)?.is_none() {
        return Err(InstallError::CompatInvalid(format!(
            "{expected_name}: нет optifine/Installer.class"
        )));
    }
    let has_mods_toml = read_entry(&mut zip, "META-INF/mods.toml", budget)?.is_some();
    let manifest = String::from_utf8_lossy(&manifest);
    let legacy_tweaker = manifest.lines().take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .any(|(name, value)| name.eq_ignore_ascii_case("TweakClass") && value.trim() == "optifine.OptiFineForgeTweaker");
    if !has_mods_toml && !legacy_tweaker {
        return Err(InstallError::CompatInvalid(format!(
            "{expected_name}: jar не выглядит совместимым с Forge"
        )));
    }
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for chunk in bytes.chunks(64 * 1024) {
        budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
        hasher.update(chunk);
    }
    Ok(http::hex(hasher.finalize()))
}

fn read_entry<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    budget: &WorkBudget,
) -> Result<Option<Vec<u8>>, InstallError> {
    let mut entry = match zip.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => {
            return Err(InstallError::CompatInvalid(format!(
                "{name}: {e}"
            )));
        }
    };
    let size = entry.size();
    if size > MAX_MARKER_BYTES {
        return Err(InstallError::CompatInvalid(format!("{name}: запись jar превышает лимит размера")));
    }
    let mut out = Vec::with_capacity(size as usize);
    budget.copy(&mut entry.by_ref().take(size + 1), &mut out)
        .map_err(|e| InstallError::CompatInvalid(format!("{name}: {e}")))?;
    if out.len() as u64 != size {
        return Err(InstallError::CompatInvalid(format!("{name}: размер записи jar не совпал")));
    }
    Ok(Some(out))
}

/// Резолв `#2847` → `14.23.5.2847` по `maven-metadata.xml`.
pub fn pick_forge_build(xml: &str, mc: &str, build: &str) -> Option<String> {
    let prefix = format!("{mc}-");
    for version in xml_versions(xml) {
        let Some(full) = version.strip_prefix(&prefix) else {
            continue;
        };
        if !is_full_version(full) {
            continue;
        }
        if full.rsplit('.').next() == Some(build) {
            return Some(full.to_string());
        }
    }
    None
}

fn xml_versions(xml: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<version>") {
        let after = &rest[start + "<version>".len()..];
        let Some(end) = after.find("</version>") else {
            break;
        };
        out.push(after[..end].trim());
        rest = &after[end + "</version>".len()..];
    }
    out
}

pub fn is_full_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 40
        && v.chars().all(|c| c.is_ascii_digit() || c == '.')
        && v.split('.').all(|p| !p.is_empty())
        && v.chars().any(|c| c.is_ascii_digit())
}

fn is_build_number(v: &str) -> bool {
    !v.is_empty() && v.len() <= 12 && v.chars().all(|c| c.is_ascii_digit())
}

pub fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.ends_with(".jar")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn adloadx_names(page: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = page;
    while let Some(pos) = rest.find("adloadx?f=") {
        rest = &rest[pos + "adloadx?f=".len()..];
        let end = rest
            .find(|c: char| matches!(c, '"' | '\'' | '&' | '<' | '>' | ' ' | '\t' | '\r' | '\n'))
            .unwrap_or(rest.len());
        let raw = &rest[..end];
        let decoded = percent_decode_str(raw).decode_utf8_lossy().into_owned();
        if is_safe_file_name(&decoded) && !names.iter().any(|n| n == &decoded) {
            names.push(decoded);
        }
        rest = &rest[end..];
    }
    names
}

fn forge_token(chunk: &str) -> Option<ForgeStatus> {
    let lower = chunk.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find("forge") {
        let start = from + p + "forge".len();
        let boundary_ok = chunk[..from + p]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if boundary_ok {
            let rest = chunk[start..].trim_start();
            let token: String = rest
                .chars()
                .take_while(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '#' | '_' | '-' | '/')
                })
                .collect();
            if token.eq_ignore_ascii_case("n/a") {
                return Some(ForgeStatus::Unavailable);
            }
            if let Some(build) = token.strip_prefix('#') {
                if is_build_number(build) {
                    return Some(ForgeStatus::Build(build.to_string()));
                }
            }
            if is_full_version(&token) {
                return Some(ForgeStatus::Full(token));
            }
        }
        from = start;
    }
    None
}

/// Минимальный декодер HTML-сущностей: в атрибутах ссылок встречаются
/// `&amp;`, кавычки и числовые ссылки. Полная таблица имён не нужна —
/// неизвестная сущность остаётся текстом как есть.
fn html_unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw.as_bytes()[i] == b'&' {
            if let Some(semi) = raw[i..].find(';') {
                if let Some(ch) = decode_entity(&raw[i + 1..i + semi]) {
                    out.push(ch);
                    i += semi + 1;
                    continue;
                }
            }
        }
        let ch = raw[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "lt" => Some('<'),
        "gt" => Some('>'),
        _ => {
            let code = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))?;
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{write_stored_zip as write_jar, TempDir};

    fn page(files: &[(&str, &str)]) -> String {
        let mut html = String::from("<html><body><table>");
        for (name, forge) in files {
            html.push_str(&format!(
                "<tr class='downloadLine'><td class='colMirror'><a href=\"adloadx?f={name}\">(Mirror)</a></td>\
                 <td class='colForge'>Forge {forge}</td></tr>"
            ));
        }
        html.push_str("</table></body></html>");
        html
    }

    #[test]
    fn downloads_pick_newest_stable_and_ignore_preview() {
        let html = page(&[
            ("preview_OptiFine_1.8.9_HD_U_M6_pre2.jar", "N/A"),
            ("OptiFine_1.8.9_HD_U_M5.jar", "#1902"),
            ("OptiFine_1.8.9_HD_U_L5.jar", "#1855"),
            ("OptiFine_1.20.1_HD_U_I6.jar", "47.2.18"),
        ]);
        let release = parse_downloads(&html, "1.8.9").expect("release");
        assert_eq!(release.file_name, "OptiFine_1.8.9_HD_U_M5.jar");
        assert_eq!(release.forge, ForgeStatus::Build("1902".into()));

        let release = parse_downloads(&html, "1.20.1").expect("release");
        assert_eq!(release.file_name, "OptiFine_1.20.1_HD_U_I6.jar");
        assert_eq!(release.forge, ForgeStatus::Full("47.2.18".into()));
    }

    #[test]
    fn downloads_require_exact_mc_and_stable_build() {
        let html = page(&[("OptiFine_1.16.5_HD_U_G8.jar", "36.2.34")]);
        assert!(parse_downloads(&html, "1.16.4").is_none());
        let only_preview = page(&[("preview_OptiFine_1.8.9_HD_U_M6_pre2.jar", "N/A")]);
        assert!(parse_downloads(&only_preview, "1.8.9").is_none());
    }

    #[test]
    fn downloads_decode_entities_and_percent_encoding() {
        let html = "<table><tr class='downloadLine'><td class='colMirror'>\
            <a href=\"adloadx?f=OptiFine%5F1.12.2_HD_U_G5.jar&amp;x=1\">Mirror</a></td>\
            <td class='colForge'>Forge #2847</td></tr></table>";
        let release = parse_downloads(html, "1.12.2").expect("release");
        assert_eq!(release.file_name, "OptiFine_1.12.2_HD_U_G5.jar");
        assert_eq!(release.forge, ForgeStatus::Build("2847".into()));
    }

    #[test]
    fn forge_is_taken_next_to_its_own_file() {
        let html = page(&[
            ("OptiFine_1.12.2_HD_U_G5.jar", "#2847"),
            ("OptiFine_1.20.1_HD_U_I6.jar", "47.2.18"),
        ]);
        let a = parse_downloads(&html, "1.12.2").expect("1.12.2");
        assert_eq!(a.forge, ForgeStatus::Build("2847".into()));
        let b = parse_downloads(&html, "1.20.1").expect("1.20.1");
        assert_eq!(b.forge, ForgeStatus::Full("47.2.18".into()));
    }

    #[test]
    fn forge_requirement_survives_duplicate_download_and_mirror_links() {
        let html = r#"<table class='downloadTable mainTable'>
            <tr class='downloadLine downloadLineMain'>
                <td class='colFile'>OptiFine HD U G5</td>
                <td class='colDownload'><a href="http://adfoc.us/serve/sitelinks/?id=475250&url=http://optifine.net/adloadx?f=OptiFine_1.12.2_HD_U_G5.jar">Download</a></td>
                <td class='colMirror'><a href="http://optifine.net/adloadx?f=OptiFine_1.12.2_HD_U_G5.jar">(Mirror)</a></td>
                <td class='colChangelog'><a href='changelog?f=OptiFine_1.12.2_HD_U_G5.jar'>Changelog</a></td>
                <td class='colForge'>Forge #2847</td>
                <td class='colDate'>24.01.2021</td>
            </tr>
        </table>"#;

        let release = parse_downloads(html, "1.12.2").expect("release");
        assert_eq!(release.file_name, "OptiFine_1.12.2_HD_U_G5.jar");
        assert_eq!(release.forge, ForgeStatus::Build("2847".into()));
    }

    #[test]
    fn forge_requirement_never_comes_from_neighboring_row() {
        let html = r#"<table>
            <tr class="downloadLine"><td class="colMirror"><a href="adloadx?f=OptiFine_1.12.2_HD_U_G5.jar">Mirror</a></td></tr>
            <tr><td class="colForge">Forge 47.2.18</td></tr>
        </table>"#;

        let release = parse_downloads(html, "1.12.2").expect("release");
        assert_eq!(release.forge, ForgeStatus::Unavailable);
    }

    #[test]
    fn forge_requirement_ignores_non_forge_columns() {
        let html = r#"<table><tr class="downloadLine">
            <td class="colMirror"><a href="adloadx?f=OptiFine_1.12.2_HD_U_G5.jar">Mirror</a></td>
            <td class="colChangelog">Forge 47.2.18</td>
        </tr></table>"#;

        let release = parse_downloads(html, "1.12.2").expect("release");
        assert_eq!(release.forge, ForgeStatus::Unavailable);
    }

    #[test]
    fn forge_requirement_stays_in_download_row_inside_nested_tables() {
        let downloads = page(&[
            ("preview_OptiFine_26.2_HD_U_K2_pre5.jar", "N/A"),
            ("OptiFine_1.12.2_HD_U_G5.jar", "#2847"),
        ]);
        let html = format!("<table class='tableRoot'><tr><td>{downloads}</td></tr></table>");

        let release = parse_downloads(&html, "1.12.2").expect("release");
        assert_eq!(release.forge, ForgeStatus::Build("2847".into()));
    }

    #[test]
    fn forge_na_means_unavailable() {
        let html = page(&[("OptiFine_26.2_HD_U_K2.jar", "N/A")]);
        let release = parse_downloads(&html, "26.2").expect("release");
        assert_eq!(release.forge, ForgeStatus::Unavailable);
    }

    #[test]
    fn token_requires_exact_file_and_hex() {
        let html = "<a href=\"downloadx?f=OptiFine_1.12.2_HD_U_G5.jar&amp;x=5e3428a98c308077f6f9fac624421ced\">Download</a>";
        assert_eq!(
            parse_token(html, "OptiFine_1.12.2_HD_U_G5.jar").as_deref(),
            Some("5e3428a98c308077f6f9fac624421ced")
        );
        assert_eq!(parse_token(html, "OptiFine_1.16.4_HD_U_G6.jar"), None);
        let no_token = "<a href=\"downloadx?f=OptiFine_1.12.2_HD_U_G5.jar&x=zzz\">x</a>";
        assert_eq!(parse_token(no_token, "OptiFine_1.12.2_HD_U_G5.jar"), None);
    }

    #[test]
    fn maven_xml_resolves_build_to_full_version() {
        let xml = r#"<metadata><versioning><versions>
            <version>1.7.10-10.13.4.1614</version>
            <version>1.12.2-14.23.5.2846</version>
            <version>1.12.2-14.23.5.2847</version>
            <version>1.12.2-14.23.5.2859</version>
            <version>1.20.1-47.2.18</version>
        </versions></versioning></metadata>"#;
        assert_eq!(
            pick_forge_build(xml, "1.12.2", "2847").as_deref(),
            Some("14.23.5.2847")
        );
        assert_eq!(
            pick_forge_build(xml, "1.20.1", "18").as_deref(),
            Some("47.2.18")
        );
        assert_eq!(pick_forge_build(xml, "1.12.2", "9999"), None);
        assert_eq!(pick_forge_build(xml, "1.8.9", "1902"), None);
    }

    fn padding() -> Vec<u8> {
        vec![0u8; (MIN_JAR_BYTES as usize) + 1024]
    }

    #[test]
    fn jar_validation_rejects_oversized_marker_members() {
        let dir = TempDir::new("optifine-large-member");
        let pad = padding();
        for marker in ["META-INF/MANIFEST.MF", "META-INF/mods.toml", "optifine/Installer.class"] {
            let path = dir.join("large.jar");
            let oversized = vec![b'x'; 1024 * 1024 + 1];
            let entries: Vec<_> = [
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n".as_slice()),
                ("META-INF/mods.toml", b"modLoader=\"javafml\"\r\n".as_slice()),
                ("optifine/Installer.class", b"\xca\xfe\xba\xbe".as_slice()),
                ("dummy.bin", pad.as_slice()),
            ].into_iter().map(|(name, bytes)| {
                (name, if name == marker { oversized.as_slice() } else { bytes })
            }).collect();
            write_jar(&path, &entries);

            assert_eq!(validate_jar(&path, "large.jar").expect_err("marker must be bounded").code(), "E_COMPAT_INVALID", "{marker}");
        }
    }

    #[test]
    fn jar_validation_refuses_a_linked_download_directory() {
        let root = TempDir::new("optifine-linked-download");
        let outside = TempDir::new("optifine-linked-outside");
        let path = outside.join("valid.jar");
        write_jar(&path, &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
            ("META-INF/mods.toml", b"modLoader=\"javafml\""),
            ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
            ("dummy.bin", &padding()),
        ]);
        crate::test_utils::link_directory(&outside, &root.join("download"));

        assert_eq!(validate_jar(&root.join("download/valid.jar"), "valid.jar")
            .expect_err("linked download directory must be rejected").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn jar_validation_does_not_treat_neoforge_metadata_as_forge_compatibility() {
        let dir = TempDir::new("optifine-neoforge-only");
        let path = dir.join("neoforge.jar");
        write_jar(&path, &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
            ("META-INF/neoforge.mods.toml", b"modLoader=\"javafml\""),
            ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
            ("dummy.bin", &padding()),
        ]);

        assert_eq!(validate_jar(&path, "neoforge.jar").expect_err("Forge metadata is required").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn jar_validation_requires_the_tweak_class_manifest_attribute() {
        let dir = TempDir::new("optifine-wrong-tweak-attribute");
        let path = dir.join("wrong.jar");
        write_jar(&path, &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\nComment: optifine.OptiFineForgeTweaker\r\nTweakClass: another.Tweaker\r\n"),
            ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
            ("dummy.bin", &padding()),
        ]);

        assert_eq!(validate_jar(&path, "wrong.jar").expect_err("mentioning the tweaker is not enough").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn jar_validation_accepts_modern_and_legacy_layouts() {
        let dir = TempDir::new("optifine-jar");
        let pad = padding();

        let modern = dir.join("modern.jar");
        write_jar(
            &modern,
            &[
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                ("META-INF/mods.toml", b"modLoader=\"javafml\"\r\n"),
                ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
                ("dummy.bin", &pad),
            ],
        );
        let sha = validate_jar(&modern, "modern.jar").expect("modern");
        assert_eq!(sha.len(), 64);

        let legacy = dir.join("legacy.jar");
        write_jar(
            &legacy,
            &[
                (
                    "META-INF/MANIFEST.MF",
                    b"Manifest-Version: 1.0\r\nTweakClass: optifine.OptiFineForgeTweaker\r\n",
                ),
                ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
                ("dummy.bin", &pad),
            ],
        );
        assert!(validate_jar(&legacy, "legacy.jar").is_ok());
    }

    #[test]
    fn jar_validation_rejects_html_and_incomplete_jars() {
        let dir = TempDir::new("optifine-bad");
        let pad = padding();

        let html = dir.join("html.jar");
        std::fs::write(&html, vec![b'<'; MIN_JAR_BYTES as usize + 1024]).expect("html");
        assert_eq!(
            validate_jar(&html, "html.jar").expect_err("html").code(),
            "E_COMPAT_INVALID"
        );

        let no_installer = dir.join("no-installer.jar");
        write_jar(
            &no_installer,
            &[
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                ("META-INF/mods.toml", b"modLoader=\"javafml\"\r\n"),
                ("dummy.bin", &pad),
            ],
        );
        assert_eq!(
            validate_jar(&no_installer, "no-installer.jar")
                .expect_err("installer")
                .code(),
            "E_COMPAT_INVALID"
        );

        let not_forge = dir.join("not-forge.jar");
        write_jar(
            &not_forge,
            &[
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
                ("dummy.bin", &pad),
            ],
        );
        assert_eq!(
            validate_jar(&not_forge, "not-forge.jar")
                .expect_err("forge")
                .code(),
            "E_COMPAT_INVALID"
        );

        let tiny = dir.join("tiny.jar");
        write_jar(
            &tiny,
            &[
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n"),
                ("META-INF/mods.toml", b"modLoader=\"javafml\"\r\n"),
                ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
            ],
        );
        assert_eq!(
            validate_jar(&tiny, "tiny.jar").expect_err("tiny").code(),
            "E_COMPAT_INVALID"
        );
    }

    #[test]
    fn download_response_must_come_from_optifine() {
        let outcome = DownloadOutcome {
            bytes: MIN_JAR_BYTES,
            content_type: "application/java-archive".into(),
            content_disposition: Some("attachment; filename=\"OptiFine_1.12.2.jar\"".into()),
            final_url: "https://evil.example/downloadx?f=x".into(),
        };
        let dir = TempDir::new("optifine-host");
        let path = dir.join("x.jar");
        std::fs::write(&path, vec![0u8; MIN_JAR_BYTES as usize]).expect("file");
        let err = validate_download(&outcome, "OptiFine_1.12.2.jar", &path)
            .expect_err("host");
        assert_eq!(err.code(), "E_COMPAT_INVALID");

        let html = DownloadOutcome {
            content_type: "text/html; charset=utf-8".into(),
            final_url: "https://optifine.net/downloadx?f=x".into(),
            ..outcome
        };
        assert_eq!(
            validate_download(&html, "OptiFine_1.12.2.jar", &path)
                .expect_err("html")
                .code(),
            "E_COMPAT_INVALID"
        );
    }

    #[test]
    fn cancelled_download_validation_cannot_accept_an_optifine_jar() {
        let temp = TempDir::new("optifine-cancelled-validation");
        let path = temp.join("OptiFine.jar");
        write_jar(&path, &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\nTweakClass: optifine.OptiFineForgeTweaker\r\n"),
            ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
            ("padding.bin", &padding()),
        ]);
        let outcome = DownloadOutcome { bytes: std::fs::metadata(&path).expect("jar").len(),
            content_type: "application/java-archive".into(), content_disposition: None,
            final_url: "https://optifine.net/downloadx".into() };
        let budget = WorkBudget::default();
        drop(budget.cancel_on_drop());

        assert!(validate_download_with_budget(&outcome, "OptiFine.jar", &path, &budget).is_err(),
            "ZIP and SHA-256 validation must use the download's cancellation budget");
    }

    /// Живой прогон: сеть и токен, только по явному запуску.
    #[tokio::test]
    #[ignore = "живой тест сети (optifine.net)"]
    async fn live_optifine_1_12_2_downloads() {
        let dir = TempDir::new("optifine-live");
        let root = crate::test_utils::compat_root(&dir);
        let budget = WorkBudget::within(std::time::Duration::from_secs(20 * 60));
        let resolved = resolve_release("1.12.2", &budget).await.expect("resolve");
        assert_eq!(resolved.forge_version, "14.23.5.2847");
        let dest = root.file(Path::new("OptiFine.jar")).expect("jar destination");
        let jar = download_jar_named(&resolved.release.file_name, &dest, None, &budget)
            .await
            .expect("download");
        assert_eq!(jar.len(), 64, "SHA-256 hex");
        assert!(dest.exists());
    }
}
