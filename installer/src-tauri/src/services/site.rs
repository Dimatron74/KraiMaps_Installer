//! Связь «установщик → сайт»: базовые URL, авторизованный HTTP-клиент,
//! маппинг HTTP-статусов в коды `E_*` для UI.
//!
//! Токены никогда не попадают в логи. Пароли через этот модуль не ходят вовсе
//! (вход — только через системный браузер + PKCE, см. `auth`).

use crate::domain::InstallError;

/// Прод-API. Переопределяется переменной окружения `KRAIMAPS_API_BASE`
/// (для локального бэкенда), иначе — прод.
pub fn api_base() -> String {
    std::env::var("KRAIMAPS_API_BASE")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "https://kraimaps.ru/api/v1".to_string())
}

/// Корень сайта (consent-страница открывается в системном браузере).
pub fn site_base() -> String {
    std::env::var("KRAIMAPS_SITE_URL")
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|_| "https://kraimaps.ru".to_string())
}

/// Единая версия установщика — берётся из `Cargo.toml` на этапе компиляции.
/// Фронтенд получает её через команду `get_installer_version`, ручных дублей
/// версии в коде нет. При релизе бампаются только манифесты:
/// `Cargo.toml` + `tauri.conf.json` + `package.json` (должны совпадать).
pub const INSTALLER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// User-Agent для запросов к сайту: `KraiMaps-Installer/{version} ({OS})`.
/// Сайт узнаёт native-сессии по префиксу `KraiMaps-Installer` и показывает
/// «Установщик • ОС» в профиле («Активные сессии») и в админке; ОС берётся
/// из скобок. Формат зафиксирован контрактом с бэкендом
/// (`SettingsService._parse_user_agent`): префикс обязателен, ОС в скобках.
pub fn installer_user_agent() -> String {
    let os = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    format!("KraiMaps-Installer/{INSTALLER_VERSION} ({os})")
}

pub fn http_client() -> Result<reqwest::Client, InstallError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(installer_user_agent())
        .build()
        .map_err(|e| InstallError::LaunchFailed(format!("http client: {e}")))
}

/// Маппинг статусов API сайта в ошибки с кодами для UI («Подробнее»).
pub async fn err_for_status(resp: reqwest::Response, ctx: &str) -> InstallError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let short = body.chars().take(300).collect::<String>();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return InstallError::AuthRequired(format!("{ctx}: 401 {short}"));
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return InstallError::AccessDenied(format!("{ctx}: 403 {short}"));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return InstallError::NotFound(format!("{ctx}: 404 {short}"));
    }
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        // 503: техработы/maintenance (nginx maintenance.html или
        // MaintenanceModeMiddleware). Отдельный тип — UI подскажет «позже».
        return InstallError::ServiceUnavailable(format!("{ctx}: 503 {short}"));
    }
    InstallError::SiteApi(format!("{ctx}: {status} {short}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_installer_prefix_and_os() {
        // Контракт с бэкендом: префикс узнаёт native-сессию, ОС в скобках.
        let ua = installer_user_agent();
        assert!(ua.starts_with("KraiMaps-Installer/"), "ua={ua}");
        assert!(ua.contains('(') && ua.ends_with(')'), "ua={ua}");
        // Версия — единая, из Cargo.toml, без ручных дублей.
        assert!(
            ua.starts_with(&format!("KraiMaps-Installer/{INSTALLER_VERSION} (")),
            "ua={ua}"
        );
    }
}
