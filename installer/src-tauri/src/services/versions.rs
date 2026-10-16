//! Резолв точной версии загрузчика для новых инстансов (D7/D10).
//!
//! Источники (официальные meta-API, только GET JSON):
//! - Forge: `promotions_slim.json` (`{mc}-recommended` → fallback `{mc}-latest`);
//! - Fabric: `meta.fabricmc.net/v2/versions/loader/{mc}` (первый stable);
//! - Quilt: `meta.quiltmc.org/v3/versions/loader/{mc}` (первый).
//! Точный хинт (`"40.2.17"`) используется как есть; диапазон (`"[40,)"`)
//! и отсутствие хинта — через API. NeoForge без точного хинта — явная
//! ошибка (угадывать запрещено).

use crate::domain::{InstallError, LoaderKind};
use crate::services::site::installer_user_agent;

/// Хинт — уже точная версия (`"40.2.17"`, `"0.19.5"`), а не диапазон.
pub fn is_exact_version(hint: &str) -> bool {
    let h = hint.trim();
    !h.is_empty()
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
        && h.chars().any(|c| c.is_ascii_digit())
}

/// Выбор версии Forge из `promotions_slim.json`: recommended-first.
/// Политика: рекомендуемая версия ветки (`{mc}-recommended`) — это
/// благословлённый стабильный билд, он есть в индексах всех лаунчеров.
/// Свежий `{mc}-latest` в индекс Modrinth App может ещё не попасть (его
/// индекс от Forge отcтаёт) — профиль с такой версией незапускаем
/// («No loader version selected»). Запасной — `{mc}-latest`. Внутри
/// одного мажора Forge обратно совместим, а карта обычно делалась на
/// версии старше обеих.
pub fn pick_forge_from_promos(doc: &serde_json::Value, mc: &str) -> Option<String> {
    let promos = doc.get("promos")?;
    for key in [format!("{mc}-recommended"), format!("{mc}-latest")] {
        if let Some(v) = promos.get(&key).and_then(|v| v.as_str()) {
            if is_exact_version(v) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Первый stable загрузчик из ответа Fabric meta.
pub fn pick_fabric_loader(doc: &serde_json::Value) -> Option<String> {
    doc.as_array()?.iter().find_map(|entry| {
        let loader = entry.get("loader")?;
        if loader.get("stable").and_then(|v| v.as_bool()).unwrap_or(false) {
            loader.get("version").and_then(|v| v.as_str()).map(|s| s.to_string())
        } else {
            None
        }
    })
}

/// Новейший стабильный загрузчик из ответа Quilt meta v3.
/// Важно: ответ НЕ отсортирован и НЕ имеет флага stable (живая проверка:
/// `[0]` — старая beta), поэтому фильтруем пре-релизы (`-beta`, `-pre`,
/// `+build`) и берём максимум по числовым компонентам.
pub fn pick_quilt_loader(doc: &serde_json::Value) -> Option<String> {
    doc.as_array()?
        .iter()
        .filter_map(|e| e.get("loader")?.get("version")?.as_str())
        .filter(|v| !v.contains('-') && !v.contains('+'))
        .max_by(|a, b| version_key(a).cmp(&version_key(b)))
        .map(|s| s.to_string())
}

/// Числовой ключ версии (`"0.26.4"` → `[0, 26, 4]`) для поиска максимума.
/// Переиспользуется выбором установщика Quilt (тот же формат версий).
pub(crate) fn version_key(v: &str) -> Vec<u64> {
    v.split('.')
        .map(|p| {
            p.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

/// Точная версия загрузчика для нового инстанса.
/// Vanilla → `None` (загрузчика нет). Сеть — только здесь, один GET за установку.
pub async fn resolve_loader_version(
    loader: LoaderKind,
    mc: &str,
    hint: Option<&str>,
) -> Result<Option<String>, InstallError> {
    if loader == LoaderKind::Vanilla {
        return Ok(None);
    }
    if let Some(h) = hint {
        if is_exact_version(h) {
            return Ok(Some(h.trim().to_string()));
        }
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(installer_user_agent())
        .build()
        .map_err(|e| InstallError::LaunchFailed(format!("http client: {e}")))?;
    match loader {
        LoaderKind::Forge => {
            let doc: serde_json::Value = client
                .get("https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json")
                .send()
                .await
                .map_err(|e| InstallError::LaunchFailed(format!("forge meta: {e}")))?
                .json()
                .await
                .map_err(|e| InstallError::LaunchFailed(format!("forge meta: {e}")))?;
            pick_forge_from_promos(&doc, mc).map(Some).ok_or_else(|| {
                InstallError::UnknownLoader(format!(
                    "нет рекомендуемой версии Forge для {mc} — выберите существующий инстанс"
                ))
            })
        }
        LoaderKind::Fabric => {
            let doc: serde_json::Value = client
                .get(format!("https://meta.fabricmc.net/v2/versions/loader/{mc}"))
                .send()
                .await
                .map_err(|e| InstallError::LaunchFailed(format!("fabric meta: {e}")))?
                .json()
                .await
                .map_err(|e| InstallError::LaunchFailed(format!("fabric meta: {e}")))?;
            pick_fabric_loader(&doc).map(Some).ok_or_else(|| {
                InstallError::UnknownLoader(format!(
                    "нет версии Fabric Loader для {mc} — выберите существующий инстанс"
                ))
            })
        }
        LoaderKind::Quilt => {
            let doc: serde_json::Value = client
                .get(format!("https://meta.quiltmc.org/v3/versions/loader/{mc}"))
                .send()
                .await
                .map_err(|e| InstallError::LaunchFailed(format!("quilt meta: {e}")))?
                .json()
                .await
                .map_err(|e| InstallError::LaunchFailed(format!("quilt meta: {e}")))?;
            pick_quilt_loader(&doc).map(Some).ok_or_else(|| {
                InstallError::UnknownLoader(format!(
                    "нет версии Quilt Loader для {mc} — выберите существующий инстанс"
                ))
            })
        }
        LoaderKind::NeoForge => Err(InstallError::UnknownLoader(
            "авторезолв NeoForge не поддерживается — укажите точную версию или выберите существующий инстанс".to_string(),
        )),
        LoaderKind::Vanilla => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_versions_pass_through() {
        assert!(is_exact_version("40.2.17"));
        assert!(is_exact_version("0.19.5"));
        assert!(!is_exact_version("[40,)"));
        assert!(!is_exact_version("latest"));
        assert!(!is_exact_version(""));
    }

    #[test]
    fn forge_prefers_recommended_over_latest() {
        let doc: serde_json::Value = serde_json::from_str(
            r#"{"promos": {"1.18.2-recommended": "40.2.0", "1.18.2-latest": "40.3.12"}}"#,
        )
        .unwrap();
        assert_eq!(
            pick_forge_from_promos(&doc, "1.18.2").as_deref(),
            Some("40.2.0")
        );
    }

    #[test]
    fn forge_falls_back_to_latest() {
        let doc: serde_json::Value =
            serde_json::from_str(r#"{"promos": {"1.18.2-latest": "40.3.12"}}"#).unwrap();
        assert_eq!(
            pick_forge_from_promos(&doc, "1.18.2").as_deref(),
            Some("40.3.12")
        );
        assert_eq!(pick_forge_from_promos(&doc, "1.99.9"), None);
    }

    #[test]
    fn fabric_picks_first_stable() {
        let doc: serde_json::Value = serde_json::from_str(
            r#"[{"loader": {"version": "0.20.0", "stable": false}},
                {"loader": {"version": "0.19.5", "stable": true}}]"#,
        )
        .unwrap();
        assert_eq!(pick_fabric_loader(&doc).as_deref(), Some("0.19.5"));
    }

    #[test]
    fn quilt_picks_max_stable_from_unsorted() {
        let doc: serde_json::Value = serde_json::from_str(
            r#"[{"loader": {"version": "0.20.0-beta.9"}},
                {"loader": {"version": "0.26.4-beta.1"}},
                {"loader": {"version": "0.25.0"}},
                {"loader": {"version": "0.26.4"}}]"#,
        )
        .unwrap();
        assert_eq!(pick_quilt_loader(&doc).as_deref(), Some("0.26.4"));
    }
}
