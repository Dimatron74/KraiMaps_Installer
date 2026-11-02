//! Реестр целей, созданных установщиком в текущем запуске.
//!
//! Слой совместимости (OptiFine) допустим только в цель, которую создал
//! установщик: существующий инстанс пользователя получает лишь карту.
//! Реестр живёт в процессе приложения — весь флоу слоя (план в temp,
//! кнопка «Повторить») тоже живёт в одном запуске.

use crate::domain::{GameTarget, LauncherKind, LoaderKind};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Идентичность цели: лаунчер + путь + имя + версии. Инстанс отличается от
/// одноимённой цели другого лаунчера и от другой версии в том же лаунчере.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TargetKey {
    launcher: LauncherKind,
    path: PathBuf,
    name: String,
    minecraft_version: String,
    loader: LoaderKind,
    loader_version: Option<String>,
}

impl TargetKey {
    fn of(target: &GameTarget) -> Self {
        Self {
            launcher: target.launcher,
            path: target.path.clone(),
            name: target.name.clone(),
            minecraft_version: target.minecraft_version.clone(),
            loader: target.loader,
            loader_version: target.loader_version.clone(),
        }
    }
}

fn registry() -> &'static Mutex<HashMap<TargetKey, HashSet<String>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<TargetKey, HashSet<String>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Пометить цель как созданную установщиком.
pub fn register(target: &GameTarget) {
    registry()
        .lock()
        .expect("provenance registry")
        .entry(TargetKey::of(target))
        .or_default();
}

/// Создана ли эта цель установщиком в текущем запуске.
pub fn was_created(target: &GameTarget) -> bool {
    registry()
        .lock()
        .expect("provenance registry")
        .contains_key(&TargetKey::of(target))
}

pub fn protect_content(target: &GameTarget, paths: Vec<String>) -> bool {
    let Ok(mut registry) = registry().lock() else {
        return false;
    };
    let Some(protected) = registry.get_mut(&TargetKey::of(target)) else {
        return false;
    };
    protected.extend(paths);
    true
}

pub fn protected_paths(target: &GameTarget) -> Vec<String> {
    registry().lock().ok()
        .and_then(|registry| registry.get(&TargetKey::of(target)).map(|paths| paths.iter().cloned().collect()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str, path: &PathBuf) -> GameTarget {
        GameTarget {
            launcher: LauncherKind::Prism,
            name: name.to_string(),
            path: path.clone(),
            minecraft_version: "1.20.1".to_string(),
            loader: LoaderKind::Fabric,
            loader_version: Some("0.15.11".to_string()),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mapinstaller-provenance-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp");
        dir
    }

    #[test]
    fn registered_target_is_recognized_as_created() {
        let dir = temp_dir("created");
        let t = target("KraiMaps", &dir);
        assert!(!was_created(&t), "до регистрации цель чужая");
        register(&t);
        assert!(was_created(&t));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_targets_stay_foreign() {
        let dir = temp_dir("foreign");
        let created = target("KraiMaps", &dir);
        register(&created);

        let other_name = target("Другая", &dir);
        assert!(!was_created(&other_name), "одноимённая папка — не та цель");

        let mut other_version = created.clone();
        other_version.minecraft_version = "1.21.1".to_string();
        assert!(!was_created(&other_version), "другая версия — не та цель");

        let mut other_loader = created.clone();
        other_loader.loader = LoaderKind::Forge;
        assert!(!was_created(&other_loader), "другой загрузчик — не та цель");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
