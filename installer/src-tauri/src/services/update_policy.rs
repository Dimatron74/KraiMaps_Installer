//! Политика обновлений: чистая логика «текущая → статус» без сети.
//!
//! Сеть может врать и пропадать, арифметика версий — нет. Поэтому решение
//! `ok / soft / hard` принимается здесь, детерминированно:
//! - текущая ниже минимальной поддерживаемой → [`UpdateStatus::Hard`]
//!   (блокировка; сервер меняет одно число — клиенты встают без пересборки);
//! - есть новее, но текущая поддерживается → [`UpdateStatus::Soft`] (оффер);
//! - иначе → [`UpdateStatus::Current`].
//! Непарсящиеся `latest`/`min` игнорируются (fail-open: лучше пропустить
//! оффер, чем заблокировать рабочего пользователя из-за битого JSON).
//! Непарсящаяся текущая — тоже [`UpdateStatus::Current`] (версия запечена
//! из `Cargo.toml`, парситься обязана; если нет — не блокируем).

use serde::Deserialize;

/// Статус версии после сверки с сервером.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Обновлений нет, версия поддерживается. Молчим.
    Current,
    /// Вышла новее, но текущая ещё поддерживается. Мягкий оффер с отказом.
    Soft { latest: String },
    /// Текущая ниже минимальной поддерживаемой. Блокировка работы.
    /// `latest=None` — сервер не предложил сборку (битый ответ): UI ведёт
    /// на ручное скачивание со страницы `/installer`, пользователь не застревает.
    Hard {
        latest: Option<String>,
        min_supported: String,
    },
}

/// Ответ динамического сервера обновлений (`GET /installer/updates`).
/// Формат Tauri (`version/url/signature/notes/pub_date`) + наше поле
/// `min_supported_version`. Лишние поля serde молча игнорирует.
/// `url/signature/pub_date` напрямую не читаем: по ним качает сам плагин
/// своим запросом — здесь они нужны только для разбора ответа.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ServerUpdateInfo {
    pub version: String,
    pub url: Option<String>,
    pub signature: Option<String>,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
    pub min_supported_version: Option<String>,
}

/// Привести сырую строку версии к виду для `semver`: trim + снять `v`-префикс
/// (Tauri допускает `v1.2.3`, крейт `semver` — нет).
pub fn normalize(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed)
}

/// Сравнить текущую версию с данными сервера.
pub fn decide(current: &str, latest: Option<&str>, min_supported: Option<&str>) -> UpdateStatus {
    let current_v = match semver::Version::parse(normalize(current)) {
        Ok(v) => v,
        // Версия запечена из Cargo.toml и обязана парситься.
        // Если нет — fail-open: лучше пропустить оффер, чем заблокировать.
        Err(_) => return UpdateStatus::Current,
    };
    let min_v = min_supported
        .map(normalize)
        .and_then(|s| semver::Version::parse(s).ok());
    if let Some(min) = min_v {
        if current_v < min {
            // Оффер кладём, только если он реально снимает блок (latest >= min).
            // Иначе кнопка «Обновить» вела бы в цикл «обновился → всё ещё
            // заблокирован»: остаётся честный ручной путь со страницы сайта.
            let latest_clean = latest
                .map(normalize)
                .filter(|s| {
                    semver::Version::parse(s).map(|v| v >= min).unwrap_or(false)
                })
                .map(str::to_string);
            return UpdateStatus::Hard {
                latest: latest_clean,
                // Возвращаем нормализованный вид для стабильного UI/логов.
                min_supported: normalize(min_supported.unwrap_or_default()).to_string(),
            };
        }
    }
    let latest_v = latest
        .map(normalize)
        .and_then(|s| semver::Version::parse(s).ok().map(|v| (v, s)));
    if let Some((v, s)) = latest_v {
        if v > current_v {
            return UpdateStatus::Soft { latest: s.to_string() };
        }
    }
    UpdateStatus::Current
}

/// Маппинг `std::env::consts::OS` в `{{target}}` апдейтера.
pub fn map_target(os: &str) -> &'static str {
    match os {
        "linux" => "linux",
        "macos" => "darwin",
        _ => "windows",
    }
}

/// Маппинг `std::env::consts::ARCH` в `{{arch}}` апдейтера.
pub fn map_arch(arch: &str) -> &'static str {
    match arch {
        "x86" => "i686",
        "aarch64" => "aarch64",
        "arm" => "armv7",
        _ => "x86_64",
    }
}

/// Конкретный URL ручной проверки (для `reqwest`): подставляет текущие
/// значения в шаблон `{{current_version}}/{{target}}/{{arch}}`.
pub fn check_url(template: &str, current: &str, target: &str, arch: &str) -> String {
    template
        .replace("{{current_version}}", current)
        .replace("{{target}}", target)
        .replace("{{arch}}", arch)
}

/// Поддерживается ли пара ОС/архитектура готовыми сборками.
/// Сейчас шипится только `windows-x86_64` (bundle-target `nsis`):
/// остальные честно отвечают «не поддерживается», а не тихо маппятся
/// на чужую платформу.
pub fn platform_supported(os: &str, arch: &str) -> bool {
    map_target(os) == "windows" && map_arch(arch) == "x86_64"
}

/// Допустима ли схема URL обновлений: только HTTPS, исключение — HTTP
/// на loopback (локальный стенд через `KRAIMAPS_UPDATES_URL`).
/// Целостность бандла всё равно держится подписью, но метаданные и
/// отпечаток версий по открытому каналу не гоняем.
pub fn https_allowed(url: &url::Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() == "http" {
        return matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("::1")
        );
    }
    false
}

/// Разобрать ответ сервера обновлений в пару `(version, min_supported)`.
/// Понимает оба формата: динамический (`version/url/signature/... +
/// `min_supported_version`) и запасной статический Tauri (`version +
/// `platforms`, без min — hard там невозможен). Мусор → `None`
/// (команда ответит `E_UPDATE_CHECK`, блокировки не будет).
pub fn parse_server_info(value: &serde_json::Value) -> Option<(String, Option<String>)> {
    let obj = value.as_object()?;
    let version = obj.get("version")?.as_str()?;
    let version = normalize(version);
    if semver::Version::parse(version).is_err() {
        return None;
    }
    let min = obj
        .get("min_supported_version")
        .and_then(serde_json::Value::as_str)
        .map(normalize)
        .filter(|s| semver::Version::parse(s).is_ok())
        .map(str::to_string);
    Some((version.to_string(), min))
}

/// Ошибка плагина связана с подписью (установка запрещена самим плагином).
/// Типизированный матчинг по вариантам — не по подстрокам в тексте.
pub fn is_signature_error(e: &tauri_plugin_updater::Error) -> bool {
    use tauri_plugin_updater::Error as PluginError;
    matches!(
        e,
        PluginError::Minisign(_)
            | PluginError::Base64(_)
            | PluginError::SignatureUtf8(_)
            | PluginError::SignedVersionMismatch { .. }
            | PluginError::MissingSignedVersion
    )
}

/// Сетевая ошибка `reqwest` означает «соединения нет» (а не «сервер плохой»).
pub fn is_offline_reqwest(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_min_is_hard() {
        assert_eq!(
            decide("0.1.0", Some("0.2.0"), Some("0.1.5")),
            UpdateStatus::Hard {
                latest: Some("0.2.0".to_string()),
                min_supported: "0.1.5".to_string(),
            }
        );
    }

    #[test]
    fn equal_min_below_latest_is_soft() {
        assert_eq!(
            decide("0.1.5", Some("0.2.0"), Some("0.1.5")),
            UpdateStatus::Soft { latest: "0.2.0".to_string() }
        );
    }

    #[test]
    fn current_latest_is_current() {
        assert_eq!(decide("0.2.0", Some("0.2.0"), Some("0.1.5")), UpdateStatus::Current);
    }

    #[test]
    fn no_latest_but_below_min_is_hard_without_offer() {
        assert_eq!(
            decide("0.1.0", None, Some("0.1.5")),
            UpdateStatus::Hard {
                latest: None,
                min_supported: "0.1.5".to_string(),
            }
        );
    }

    #[test]
    fn garbage_latest_is_ignored_fail_open() {
        assert_eq!(decide("0.1.0", Some("not-a-version"), Some("0.1.0")), UpdateStatus::Current);
    }

    #[test]
    fn garbage_min_is_ignored_latest_still_honored() {
        assert_eq!(
            decide("0.1.0", Some("0.2.0"), Some("???")),
            UpdateStatus::Soft { latest: "0.2.0".to_string() }
        );
    }

    #[test]
    fn v_prefix_and_whitespace_are_tolerated() {
        assert_eq!(
            decide(" v0.1.0 ", Some("v0.2.0"), Some(" v0.1.5 ")),
            UpdateStatus::Hard {
                latest: Some("0.2.0".to_string()),
                min_supported: "0.1.5".to_string(),
            }
        );
    }

    #[test]
    fn garbage_current_is_fail_open() {
        assert_eq!(decide("???", Some("0.2.0"), Some("0.1.5")), UpdateStatus::Current);
    }

    #[test]
    fn target_arch_mapping() {
        assert_eq!(map_target("windows"), "windows");
        assert_eq!(map_target("linux"), "linux");
        assert_eq!(map_target("macos"), "darwin");
        assert_eq!(map_arch("x86_64"), "x86_64");
        assert_eq!(map_arch("aarch64"), "aarch64");
    }

    #[test]
    fn check_url_substitutes_placeholders() {
        let url = check_url(
            "https://kraimaps.ru/api/v1/installer/updates?current={{current_version}}&target={{target}}&arch={{arch}}",
            "0.1.1",
            "windows",
            "x86_64",
        );
        assert_eq!(
            url,
            "https://kraimaps.ru/api/v1/installer/updates?current=0.1.1&target=windows&arch=x86_64"
        );
    }

    #[test]
    fn hard_with_older_latest_offers_nothing() {
        // Битая конфигурация сервера (latest ниже min): не заманиваем
        // «обновлением», которое не снимет блок, — только ручной путь.
        assert_eq!(
            decide("0.1.0", Some("0.1.2"), Some("0.1.5")),
            UpdateStatus::Hard {
                latest: None,
                min_supported: "0.1.5".to_string(),
            }
        );
    }

    #[test]
    fn hard_with_same_as_current_latest_offers_nothing() {
        assert_eq!(
            decide("0.1.0", Some("0.1.0"), Some("0.1.5")),
            UpdateStatus::Hard {
                latest: None,
                min_supported: "0.1.5".to_string(),
            }
        );
    }

    #[test]
    fn hard_with_reaching_latest_keeps_offer() {
        // latest дотягивает до min — оффер валиден, ведёт к разблокировке.
        assert_eq!(
            decide("0.1.0", Some("0.2.0"), Some("0.1.5")),
            UpdateStatus::Hard {
                latest: Some("0.2.0".to_string()),
                min_supported: "0.1.5".to_string(),
            }
        );
    }

    #[test]
    fn prerelease_is_below_release() {
        assert_eq!(
            decide("1.0.0-beta.1", Some("1.0.0"), None),
            UpdateStatus::Soft { latest: "1.0.0".to_string() }
        );
    }

    #[test]
    fn stale_cache_above_latest_is_current() {
        // Протухший CDN/кэш с версией новее текущей: даунгрейдов не предлагаем.
        assert_eq!(decide("0.3.0", Some("0.2.0"), None), UpdateStatus::Current);
    }

    #[test]
    fn only_windows_x64_is_supported() {
        assert!(platform_supported("windows", "x86_64"));
        assert!(!platform_supported("linux", "x86_64"));
        assert!(!platform_supported("windows", "aarch64"));
        assert!(!platform_supported("macos", "aarch64"));
    }

    #[test]
    fn https_gate_allows_only_https_and_loopback_http() {
        let https = url::Url::parse("https://kraimaps.ru/api/v1/installer/updates").unwrap();
        assert!(https_allowed(&https));
        let evil = url::Url::parse("http://evil.example/updates").unwrap();
        assert!(!https_allowed(&evil));
        let local = url::Url::parse("http://localhost:8080/updates").unwrap();
        assert!(https_allowed(&local));
        let loopback = url::Url::parse("http://127.0.0.1:8080/updates").unwrap();
        assert!(https_allowed(&loopback));
    }

    #[test]
    fn dynamic_shape_parses_with_min() {
        let v: serde_json::Value = serde_json::json!({
            "version": "0.2.0",
            "url": "https://files.kraimaps.ru/installer/0.2.0/app.exe",
            "signature": "abc",
            "notes": "fix",
            "min_supported_version": "0.1.5",
        });
        assert_eq!(
            parse_server_info(&v),
            Some(("0.2.0".to_string(), Some("0.1.5".to_string())))
        );
    }

    #[test]
    fn static_shape_fallback_parses_without_min() {
        // Запасной updater.json: версия есть, min нет — hard невозможен.
        let v: serde_json::Value = serde_json::json!({
            "version": "0.2.0",
            "pub_date": "2026-10-10T00:00:00Z",
            "platforms": {"windows-x86_64": {"url": "https://x/y.exe", "signature": "abc"}},
        });
        assert_eq!(parse_server_info(&v), Some(("0.2.0".to_string(), None)));
    }

    #[test]
    fn garbage_shape_returns_none() {
        let v: serde_json::Value = serde_json::json!({"platforms": {}});
        assert_eq!(parse_server_info(&v), None);
        let v: serde_json::Value = serde_json::json!({"version": "???", "url": "https://x"});
        assert_eq!(parse_server_info(&v), None);
    }

    #[test]
    fn signature_errors_classified_by_variant() {
        use tauri_plugin_updater::Error as PluginError;
        assert!(is_signature_error(&PluginError::SignatureUtf8("xxx".to_string())));
        assert!(!is_signature_error(&PluginError::ReleaseNotFound));
        assert!(!is_signature_error(&PluginError::TargetNotFound("windows-x86_64".to_string())));
        assert!(!is_signature_error(&PluginError::EmptyEndpoints));
    }

    #[tokio::test]
    async fn refused_connection_is_offline() {
        // Закрытый порт = детерминированный отказ в соединении, без моков.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/updates")
            .send()
            .await
            .expect_err("порт 1 должен быть закрыт");
        assert!(is_offline_reqwest(&err), "got: {err}");
    }
}
