//! Жизненный цикл временных файлов установщика.
//!
//! Правило владения: временный файл удаляет тот флоу, который его создал,
//! сразу как файл стал не нужен ([`discard_install_files`] после успешной
//! установки). [`sweep_stale_temp`] при старте — страховка от крашей/убийств
//! процесса, а не основной механизм: метёт только allowlist имён старше
//! [`STALE_AFTER`], занятое молча пропускает, старт никогда не роняет.
//!
//! Чужие файлы не трогаем никогда: корень Temp wildcard'ом не метём, только
//! точные наши префиксы (ср. `Modrinth App-*-updater-*` — чужой мусор
//! апдейтера Modrinth, его оставляем как есть).

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Возраст, после которого временный файл считается мусором.
/// С запасом покрывает самую долгую операцию (провижининг версии —
/// до 30 минут) и вторую копию приложения.
pub const STALE_AFTER: Duration = Duration::from_secs(6 * 3600);

/// Потолок удалений за один проход — sweep обязан быть миллисекундным.
const MAX_REMOVALS_PER_SWEEP: usize = 200;

/// Наш подкаталог в системном temp (карты, иконки, пробы).
const MANAGED_DIR: &str = "kraimaps";

/// Итог прохода (для логов/тестов; на UI не влияет).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub scanned: usize,
    pub removed: usize,
    pub skipped: usize,
}

/// Точечное удаление файлов завершённой установки (архив карты, иконки).
/// Принимает только строго наши имена внутри системного
/// temp — чужое и внешнее отклоняется. Best-effort: возвращает число
/// удалённых, не падает никогда.
pub fn discard_install_files(paths: Vec<String>) -> usize {
    let tmp = std::env::temp_dir();
    paths
        .iter()
        .filter_map(|p| validated_temp_file(&tmp, p))
        .filter(|p| std::fs::remove_file(p).is_ok())
        .count()
}

/// Путь — файл внутри системного temp (без `..`-побегов, глубина ≤ 2)
/// с нашим именем. Иначе `None`.
fn validated_temp_file(tmp: &Path, raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    let rel = path.strip_prefix(tmp).ok()?;
    // `..` в середине обходит strip_prefix текстовым сравнением —
    // такие пути отклоняем явно.
    let mut depth = 0usize;
    for comp in rel.components() {
        match comp {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            _ => return None,
        }
    }
    if depth == 0 || depth > 2 {
        return None;
    }
    let name = path.file_name()?.to_string_lossy();
    if is_install_file(&name) {
        Some(tmp.join(rel))
    } else {
        None
    }
}

/// Имена файлов, которые флоу установки вправе удалить за собой:
/// скачанные карты и временные иконки.
fn is_install_file(name: &str) -> bool {
    if name.starts_with("map_") {
        return name.ends_with(".zip") || name.ends_with(".mrpack");
    }
    if name.starts_with("icon_") {
        return name.ends_with(".png");
    }
    false
}

/// Классификация записей корня Temp, которыми владеем мы.
#[derive(Debug, PartialEq, Eq)]
enum ManagedRoot {
    /// Рабочий каталог провижининга (`mapinstaller-tl-<pid>-<nanos>`).
    TlWorkdir,
    /// Наследие старых версий: бэкапы app.db в Temp (переехали рядом
    /// с данными, см. `backup_app_db`).
    AppDbLegacy,
    /// Распаковка обновления Tauri-плагином (`MapInstaller KraiMaps-*-updater-*`).
    UpdaterDir,
    /// Каталоги официального JVM-установщика Forge (`forge_installer*`):
    /// создаёт не наш код, а запущенный нами процесс; метём только по
    /// возрасту — свежий чужой прогон не задеваем.
    ForgeInstaller,
}

fn classify_root(name: &str, is_dir: bool) -> Option<ManagedRoot> {
    if !is_dir {
        return None;
    }
    if name.starts_with("mapinstaller-tl-") {
        Some(ManagedRoot::TlWorkdir)
    } else if name.starts_with("mapinstaller-appdb-") {
        Some(ManagedRoot::AppDbLegacy)
    } else if name.starts_with("MapInstaller KraiMaps-") && name.contains("-updater-") {
        Some(ManagedRoot::UpdaterDir)
    } else if name.starts_with("forge_installer") {
        Some(ManagedRoot::ForgeInstaller)
    } else {
        None
    }
}

/// Наши имена внутри `kraimaps/`: других писателей у каталога нет,
/// но префиксы проверяем всё равно (оборона в глубину).
fn is_managed_child(name: &str) -> bool {
    name.starts_with("map_") || name.starts_with("icon_") || name.starts_with("rp_probe_")
}

/// Стартовый проход по мусору. Один `readdir` корня + один `readdir
/// kraimaps/`, stat только кандидатам. Ошибки, занятые файлы и свежие
/// записи — в пропуск, не в ошибку.
pub fn sweep_stale_temp() -> SweepReport {
    let mut report = SweepReport::default();
    let tmp = std::env::temp_dir();
    let Ok(root) = std::fs::read_dir(&tmp) else {
        return report;
    };
    for entry in root.flatten() {
        if report.removed >= MAX_REMOVALS_PER_SWEEP {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if classify_root(&name, is_dir).is_none() {
            continue;
        }
        report.scanned += 1;
        sweep_one(&entry.path(), is_dir, &mut report);
    }
    let managed = tmp.join(MANAGED_DIR);
    if let Ok(children) = std::fs::read_dir(&managed) {
        for entry in children.flatten() {
            if report.removed >= MAX_REMOVALS_PER_SWEEP {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_managed_child(&name) {
                continue;
            }
            report.scanned += 1;
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            sweep_one(&entry.path(), is_dir, &mut report);
        }
    }
    report
}

fn sweep_one(path: &Path, is_dir: bool, report: &mut SweepReport) {
    if !is_stale(path) {
        report.skipped += 1;
        return;
    }
    let ok = if is_dir {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        std::fs::remove_file(path).is_ok()
    };
    if ok {
        report.removed += 1;
    } else {
        // Занято другим процессом / нет прав — попробуем при следующем старте.
        report.skipped += 1;
    }
}

/// Протухший — mtime старше порога. Будущее время и ошибки stat считаем
/// свежими (безопасное направление).
fn is_stale(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    mtime.elapsed().map(|age| age >= STALE_AFTER).unwrap_or(false)
}

/// Ротация каталогов-бэкапов (`<parent>/<prefix><stamp>-<pid>`): оставляет
/// `keep` самых свежих по mtime, остальные удаляет. Best-effort.
pub fn prune_old_backups(parent: &Path, prefix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut dirs: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && e.file_name().to_string_lossy().starts_with(prefix)
        })
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (mtime, e.path())
        })
        .collect();
    // Самые свежие — в начале; всё за пределами keep — под удаление.
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in dirs.into_iter().skip(keep) {
        std::fs::remove_dir_all(&path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_file_allowlist_is_tight() {
        // Наше — пропускаем.
        for ok in [
            "map_42_Tower.zip",
            "map_1_x.mrpack",
            "icon_123_456.png",
            "icon_cvt_1_2.png",
        ] {
            assert!(is_install_file(ok), "{ok}");
        }
        // Чужое и подозрительное — отклоняем.
        for bad in [
            "",
            "map_42.exe",
            "map_42.zip.exe",
            "icon.png",
            "notes.txt",
            "mapinstaller-evil.exe",
            ".mrpack",
            "MAP_42_X.ZIP",
        ] {
            assert!(!is_install_file(bad), "{bad}");
        }
    }

    #[test]
    fn root_classifier_ignores_foreign_names() {
        // Наше.
        assert!(classify_root("mapinstaller-tl-1-2", true).is_some());
        assert!(classify_root("mapinstaller-appdb-1-2", true).is_some());
        assert!(classify_root("MapInstaller KraiMaps-0.1.2-updater-XyZ123", true).is_some());
        assert!(classify_root("forge_installer12345", true).is_some());
        // Чужое — руки прочь (в т.ч. апдейтеры Modrinth и файлы).
        assert_eq!(classify_root("Modrinth App-0.21.6-updater-OJoKHi", true), None);
        assert_eq!(classify_root("mapinstaller-x.mrpack", false), None);
        assert_eq!(classify_root("mapinstaller-tl-1-2", false), None);
        assert_eq!(classify_root("kraimaps", true), None);
        assert_eq!(classify_root("some_random_dir", true), None);
    }

    #[test]
    fn traversal_paths_are_rejected() {
        let tmp = std::env::temp_dir();
        let evil = tmp
            .join("kraimaps")
            .join("..")
            .join("Windows")
            .join("map_1_x.zip");
        assert_eq!(
            validated_temp_file(&tmp, &evil.to_string_lossy()),
            None,
            "..-побег отклоняем"
        );
        assert_eq!(validated_temp_file(&tmp, ""), None);
        // Вне temp — отклоняем, даже с нашим именем.
        let outside = std::env::current_dir()
            .expect("cwd")
            .join("map_1_x.zip")
            .to_string_lossy()
            .into_owned();
        assert_eq!(validated_temp_file(&tmp, &outside), None);
    }

    #[test]
    fn discard_removes_only_managed_files() {
        // validated_temp_file привязан к реальному temp: проверяем
        // allowlist + удаление на pid-уникальных путях (чужой подчищаем).
        let tmp = std::env::temp_dir();
        let ours = tmp.join(format!("map_999_probe_{}.zip", std::process::id()));
        let foreign = tmp.join(format!("probe_notes_{}.txt", std::process::id()));
        std::fs::write(&ours, b"x").expect("write");
        std::fs::write(&foreign, b"x").expect("write");
        let removed = discard_install_files(vec![
            ours.to_string_lossy().into_owned(),
            foreign.to_string_lossy().into_owned(),
            String::new(),
        ]);
        assert_eq!(removed, 1);
        assert!(!ours.exists(), "наш файл удалён");
        assert!(foreign.exists(), "чужой файл цел");
        std::fs::remove_file(&foreign).ok();
    }

    #[test]
    fn prune_keeps_newest_three() {
        let parent = std::env::temp_dir().join(format!(
            "mapinstaller-prune-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&parent).expect("parent");
        for i in 0..5 {
            let d = parent.join(format!("appdb-test-{i}"));
            std::fs::create_dir_all(&d).expect("child");
            // Разносим mtime, чтобы порядок был детерминирован.
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        // Посторонний каталог с тем же корнем имени, но другим префиксом.
        std::fs::create_dir_all(parent.join("other-1")).expect("other");
        prune_old_backups(&parent, "appdb-", 3);
        let left: Vec<String> = std::fs::read_dir(&parent)
            .expect("read")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 4, "3 свежих + посторонний: {left:?}");
        assert!(left.contains(&"other-1".to_string()));
        assert!(left.contains(&"appdb-test-4".to_string()));
        assert!(!left.contains(&"appdb-test-0".to_string()));
        std::fs::remove_dir_all(&parent).ok();
    }
}
