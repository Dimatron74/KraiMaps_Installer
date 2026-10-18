//! TLauncher: общий game dir без инстансов.
//!
//! Версии — папки `versions/<Имя>/` (`<Имя>.json` + `<Имя>.jar` + `natives/`).
//! Имя в списке TL = имя папки (= `id` в json), отдельного display name нет.
//! Наши версии помечаем суффиксом ` [KK]` (только созданные нами — чужое
//! не переименовываем). Выбор версии — `login.version.game` в
//! `tlauncher-2.0.properties` (правим только при закрытом TL: лаунчер
//! перезаписывает конфиг при выходе).
//!
//! Всё общее с Legacy Launcher и официальным лаунчером вынесено
//! в `vanilla`; здесь только то, что свойственно именно TLauncher.

use super::vanilla::{self, PreselectSpec};
use crate::domain::{InstallError, LauncherKind};
use crate::services::deploy;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Свойства TLauncher (лежат в `%APPDATA%/.tlauncher/`, НЕ в game dir).
const PROPS_FILE: &str = "tlauncher-2.0.properties";
/// Ключ выбранной версии в этих свойствах.
const PROPS_VERSION_KEY: &str = "login.version.game";
/// Ключ с game dir — по нему находим корень, если он нетипичный.
const PROPS_GAMEDIR_KEY: &str = "minecraft.gamedir";

/// Спецификация предвыбора версии для TLauncher.
fn preselect_spec(roaming: &Path) -> PreselectSpec {
    PreselectSpec {
        props_dir: roaming.join(".tlauncher"),
        file_name: PROPS_FILE,
        key: PROPS_VERSION_KEY,
    }
}

/// Корень данных TLauncher: `versions/` + сигнал TL. Голый `versions/` без
/// сигналов — это официальный лаунчер или Legacy Launcher, не наш клиент.
///
/// Маркером служит ТОЛЬКО `TLauncher*.exe` в корне данных: `tlauncher_profiles.json`
/// лежит и у Legacy Launcher (он держит там профили аккаунтов), а Windows не
/// различает регистр имён — с этим маркером папка игры Legacy распознавалась
/// как TLauncher.
pub fn is_tlauncher_data_dir(p: &Path) -> bool {
    p.is_dir() && p.join("versions").is_dir() && find_tlauncher_exe(p).is_some()
}

/// Exe лаунчера рядом с данными (для кнопки «Играть»).
pub fn find_tlauncher_exe(data_dir: &Path) -> Option<PathBuf> {
    for exact in ["TLauncher.exe", "TLauncher-win7.exe", "TLauncher32bit.exe"] {
        let c = data_dir.join(exact);
        if c.is_file() {
            return Some(c);
        }
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(data_dir)
        .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    names.into_iter().find(|p| {
        p.is_file()
            && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
            && p.file_stem()
                .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("tlauncher"))
    })
}

/// «Играть» для TLauncher: стабильного CLI нет (потолок лаунчера) —
/// открываем приложение, пользователь жмёт Play на версии.
pub fn launch_tlauncher(exe: &Path) -> Result<String, InstallError> {
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| InstallError::LaunchFailed(format!("{}: {e}", exe.display())))?;
    Ok(exe.display().to_string())
}

/// Запущен ли TLauncher: java-процессы, чей exe лежит под `.tlauncher\starter`
/// (лаунчер; игра — из Mojang-рантайма, её не трогаем).
///
/// Именно `starter`, а не просто `.tlauncher`: Legacy Launcher тоже
/// java-процесс под `.tlauncher` (его JRE лежит в папке установки), и без
/// этого уточнения оба лаунчера выглядели бы одинаково.
pub(crate) fn is_launcher_process(exe: Option<&Path>) -> bool {
    exe.is_some_and(|e| {
        let s = e.to_string_lossy().to_lowercase().replace('/', "\\");
        s.contains(".tlauncher\\starter")
    })
}

pub(crate) fn tlauncher_running() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes()
        .values()
        .any(|p| java_process(p) && is_launcher_process(p.exe()))
}

fn java_process(proc: &sysinfo::Process) -> bool {
    let name = proc.name().to_string_lossy().to_lowercase();
    name == "java.exe" || name == "javaw.exe" || name == "java"
}

/// Аккуратно закрывает TLauncher перед правкой версий/конфига (иначе
/// лаунчер перезапишет properties при выходе). Просим процессы завершиться,
/// ждём до ~10 с. `false` — не ошибка, установка продолжается, но предвыбор
/// версии пропускается.
pub fn close_tlauncher() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    for _ in 0..40 {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut alive = false;
        for proc in sys.processes().values() {
            if java_process(proc) && is_launcher_process(proc.exe()) {
                alive = true;
                proc.kill();
            }
        }
        if !alive {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    !tlauncher_running()
}

/// Цели TLauncher: по одной на папку версии; `path` — сам game dir
/// (общий для всех версий, инстансов нет).
pub fn enumerate_targets(data_dir: &Path) -> Vec<crate::domain::GameTarget> {
    vanilla::enumerate_targets(data_dir, data_dir, LauncherKind::Tlauncher)
}

/// Подготовка версии TLauncher: поиск → провижининг официальным
/// установщиком → пометка [KK] → предвыбор в конфиге.
pub async fn ensure_version(
    data_dir: &Path,
    roaming: Option<&Path>,
    mc_version: &str,
    loader: crate::domain::LoaderKind,
    loader_version: Option<&str>,
    progress: deploy::Progress<'_>,
) -> Result<vanilla::EnsureResult, InstallError> {
    if mc_version.trim().is_empty() {
        return Err(InstallError::UnknownLoader(
            "неизвестна версия игры для подготовки".to_string(),
        ));
    }
    let loader_version = vanilla::require_exact_loader_version(loader, loader_version)?;

    // Версия уже есть (помеченная или обычная — матчим по паре).
    if let Some(found) = vanilla::find_match(data_dir, mc_version, loader, &loader_version) {
        let _closed = close_tlauncher();
        return finish(data_dir, roaming, mc_version, loader, loader_version, found.dir_name);
    }

    let _closed = close_tlauncher();
    let roots = crate::services::java::JavaRoots {
        mojang: vec![data_dir.join("runtime")],
        bundled: roaming
            .map(|r| vec![r.join(".tlauncher").join("starter")])
            .unwrap_or_default(),
    };
    let marked = vanilla::provision(
        data_dir,
        mc_version,
        loader,
        &loader_version,
        &roots,
        progress,
    )
    .await?;
    finish(data_dir, roaming, mc_version, loader, loader_version, marked)
}

/// Финал ensure: цель + предвыбор версии (только при закрытом TL) +
/// флаг пустоты `mods/`.
fn finish(
    data_dir: &Path,
    roaming: Option<&Path>,
    mc: &str,
    loader: crate::domain::LoaderKind,
    loader_version: String,
    dir_name: String,
) -> Result<vanilla::EnsureResult, InstallError> {
    let mut preselected = false;
    if let Some(roaming) = roaming {
        // TL перезаписывает properties при выходе — правим только
        // когда лаунчер точно закрыт, иначе молча пропускаем.
        if !tlauncher_running() {
            preselected = vanilla::set_property(
                &preselect_spec(roaming),
                &dir_name,
            )
            .unwrap_or(false);
        }
    }
    Ok(vanilla::EnsureResult {
        target: crate::domain::GameTarget {
            launcher: LauncherKind::Tlauncher,
            name: dir_name,
            path: data_dir.to_path_buf(),
            minecraft_version: mc.to_string(),
            loader,
            loader_version: Some(loader_version),
        },
        mods_empty: vanilla::mods_empty(data_dir),
        preselected,
    })
}

/// Чтение `minecraft.gamedir` из конфига — корень может быть нетипичным.
pub fn game_dir_from_props(roaming: &Path) -> Option<PathBuf> {
    let props = roaming.join(".tlauncher").join(PROPS_FILE);
    let text = std::fs::read_to_string(props).ok()?;
    let dir = vanilla::property_value(&text, PROPS_GAMEDIR_KEY)?;
    let dir = PathBuf::from(dir);
    is_tlauncher_data_dir(&dir).then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-tlauncher-{tag}"))
    }

    #[test]
    fn detection_markers_require_tl_signals() {
        let root = temp_root("markers");
        // Голый versions/ без сигналов TL — не TLauncher (официальный лаунчер).
        std::fs::create_dir_all(root.join("versions")).expect("versions");
        assert!(!is_tlauncher_data_dir(&root));
        // Сигнал TLauncher — exe в корне данных (живой кейс пользователя).
        std::fs::write(root.join("TLauncher.exe"), b"fake").expect("exe");
        assert!(is_tlauncher_data_dir(&root));
        assert_eq!(
            find_tlauncher_exe(&root),
            Some(root.join("TLauncher.exe"))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn legacy_profiles_file_is_not_a_tlauncher_signal() {
        // Живой случай: папка игры Legacy Launcher содержит
        // `tlauncher_profiles.json` (профили аккаунтов) и `versions/`, но
        // TLauncher в ней нет. Раньше такой каталог проходил как TLauncher.
        let root = temp_root("not-tl");
        std::fs::create_dir_all(root.join("versions")).expect("versions");
        std::fs::write(root.join("tlauncher_profiles.json"), "{}").expect("profiles");
        assert!(!is_tlauncher_data_dir(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn launcher_process_is_distinguished_from_legacy() {
        // JRE TLauncher лежит под `starter`, JRE Legacy — в папке установки
        // лаунчера. Оба процесса — java под `.tlauncher`.
        let tl = PathBuf::from(r"C:\Users\X\AppData\Roaming\.tlauncher\starter\jre\bin\javaw.exe");
        let legacy =
            PathBuf::from(r"C:\Users\X\AppData\Roaming\.tlauncher\legacy\Minecraft\jre\x64\bin\javaw.exe");
        let java = PathBuf::from(r"C:\Program Files\Java\jre\bin\javaw.exe");
        assert!(is_launcher_process(Some(&tl)));
        assert!(!is_launcher_process(Some(&legacy)));
        assert!(!is_launcher_process(Some(&java)));
        assert!(!is_launcher_process(None));
    }

    #[test]
    fn enumerate_uses_game_dir_as_target_path() {
        let root = temp_root("enum");
        let versions = root.join("versions");
        let good = versions.join("fabric-loader-0.19.5-1.20.1");
        std::fs::create_dir_all(&good).expect("good");
        std::fs::write(
            good.join("fabric-loader-0.19.5-1.20.1.json"),
            r#"{"id": "fabric-loader-0.19.5-1.20.1",
                "inheritsFrom": "1.20.1",
                "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries": [{"name": "net.fabricmc:fabric-loader:0.19.5"}]}"#,
        )
        .expect("json");

        let targets = enumerate_targets(&root);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].launcher, LauncherKind::Tlauncher);
        assert_eq!(targets[0].path, root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn game_dir_is_read_from_properties() {
        let root = temp_root("gamedir");
        let props_dir = root.join(".tlauncher");
        std::fs::create_dir_all(&props_dir).expect("props");
        // Игровой каталог с exe TLauncher внутри.
        let game = root.join("custom-game");
        std::fs::create_dir_all(game.join("versions")).expect("versions");
        std::fs::write(game.join("TLauncher.exe"), b"fake").expect("exe");
        let escaped = game
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace(':', "\\:");
        std::fs::write(
            props_dir.join(PROPS_FILE),
            format!("minecraft.gamedir={escaped}\n"),
        )
        .expect("props write");

        assert_eq!(game_dir_from_props(&root), Some(game));
        std::fs::remove_dir_all(&root).ok();
    }
}