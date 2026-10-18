//! Legacy Launcher (он же TLauncher Legacy, «TL Legacy»).
//!
//! Формат версий — как у TLauncher: `versions/<Имя>/{<Имя>.json, <Имя>.jar,
//! natives/}` в папке игры (`minecraft.gamedir`). Отличие в том, что лаунчер
//! умеет раскладывать `saves/`/`mods/` по подпапкам — три режима в ключе
//! `minecraft.gamedir.separate`:
//!
//! | значение | где лаунчер запустит игру |
//! |---|---|
//! | `none` | прямо в папке игры |
//! | `family` | `<папка игры>/home/<семейство из json версии>` |
//! | `version` | `<папка игры>/home/<Имя версии>` |
//!
//! Отсюда главное, что нельзя упустить: **файлы игры и каталог пользователя —
//! разные папки**. `versions/`, `libraries/`, `assets/` лежат в корне, туда же
//! надо направить установщик Forge/Fabric (это прямо сказано в вики лаунчера:
//! «указывайте в установщиках ту же папку, что и в пункте Директория»), а
//! `saves/`/`mods/`/`config/` — в подпапку режима. Поэтому здесь два пути:
//! `game_dir` для провижининга и `target_dir` для деплоя.
//!
//! Каждая подпапка режима — полностью самостоятельный игровой каталог
//! (проверено: `home/NeoForge-1.21` содержит свои `mods`, `config`,
//! `defaultconfigs`, `.cache`, `saves`, `options.txt`), то есть работа с ней
//! как с инстансом Prism, а не как с общей папкой.

use super::vanilla::{self, PreselectSpec};
use crate::domain::{GameTarget, InstallError, LauncherKind, LoaderKind};
use crate::services::deploy;
use std::path::{Path, PathBuf};

/// Конфиг лаунчера. В портативной сборке лежит рядом с exe
/// (`<папка установки>/tl.properties`), у обычной — `%APPDATA%\.tlauncher\`.
const PROPS_FILE: &str = "tl.properties";
const PROPS_VERSION_KEY: &str = "login.version";
const PROPS_GAMEDIR_KEY: &str = "minecraft.gamedir";
const PROPS_SEPARATE_KEY: &str = "minecraft.gamedir.separate";
const PROPS_JRE_DIR_KEY: &str = "minecraft.jre.dir";
/// Сигналы папки установки лаунчера (по exe и bootstrap).
const INSTALL_MARKER_EXE: &str = "LL.exe";
const INSTALL_MARKER_BOOTSTRAP: &str = "launcher/bootstrap.jar";

/// Режим подпапок `minecraft.gamedir.separate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeparateDirs {
    None,
    Family,
    Version,
}

impl SeparateDirs {
    /// Разбор значения из конфига. Неизвестное и пустое — `None`: лаунчер
    /// тоже падает на негодном значении, а мы не имеем права угадывать.
    pub fn parse(raw: Option<&str>) -> SeparateDirs {
        match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "family" => SeparateDirs::Family,
            "version" => SeparateDirs::Version,
            _ => SeparateDirs::None,
        }
    }
}

/// Конфиг Legacy Launcher, который нам нужен.
#[derive(Debug, Clone)]
pub struct LegacyConfig {
    /// Папка игры (`minecraft.gamedir`) — корень с `versions/` и `libraries/`.
    pub game_dir: PathBuf,
    /// Папка установки лаунчера (рядом с `LL.exe`) — там лежит `tl.properties`.
    pub install_dir: PathBuf,
    /// Настроенный режим подпапок.
    pub separate: SeparateDirs,
}

impl LegacyConfig {
    /// Читает `tl.properties`. `install_dir` — кандидат на папку установки
    /// лаунчера. `None` — конфига нет или он нечитаем.
    ///
    /// Принимает и путь к `LL.exe`: наружу мы отдаём `exe_path`, его легко
    /// передать сюда как есть, а отличается он от папки одним компонентом
    /// пути, и ошибка выглядит как «нет конфига». Проверяем `is_dir`, а не
    /// `is_file`, потому что `LL.exe` может быть ещё не скачан, — тогда путь
    /// не существует вовсе и `is_file` дал бы ложный ответ «это папка».
    pub fn load(install_dir: &Path) -> Option<LegacyConfig> {
        let install_dir = if install_dir.is_dir() {
            install_dir
        } else {
            install_dir.parent()?
        };
        let text = std::fs::read_to_string(install_dir.join(PROPS_FILE))
            .or_else(|_| std::fs::read_to_string(install_dir.join("src").join(PROPS_FILE)))
            .ok()?;
        let game_dir = PathBuf::from(vanilla::property_value(&text, PROPS_GAMEDIR_KEY)?);
        if !is_game_dir(&game_dir) {
            return None;
        }
        Some(LegacyConfig {
            game_dir,
            install_dir: install_dir.to_path_buf(),
            separate: SeparateDirs::parse(
                vanilla::property_value(&text, PROPS_SEPARATE_KEY).as_deref(),
            ),
        })
    }

    /// Папка, куда лаунчер положит `saves/`/`mods/` для этой версии.
    // `family` в json — это то, по чему лаунчер раскладывает папки; считать
    // его самим здесь нельзя (не совпадём — моды лягут не туда), поэтому
    // версия без поля для FAMILY-режима непригодна.
    pub fn target_dir(
        &self,
        version_dir_name: &str,
        family: Option<&str>,
    ) -> Result<PathBuf, InstallError> {
        match self.separate {
            SeparateDirs::None => Ok(self.game_dir.clone()),
            SeparateDirs::Version => Ok(self.home_dir().join(version_dir_name)),
            SeparateDirs::Family => {
                let family = family.ok_or_else(|| InstallError::NoDataDir {
                    launcher: format!(
                        "Legacy Launcher (у версии «{version_dir_name}» нет поля family — запустите её в лаунчере один раз)"
                    ),
                })?;
                Ok(self.home_dir().join(family))
            }
        }
    }

    fn home_dir(&self) -> PathBuf {
        self.game_dir.join("home")
    }

    /// Папка для ванильной версии. В отличие от [`Self::target_dir`] ошибкой
    /// не кончается: версии может ещё не быть на диске — Legacy держит
    /// нескачанные версии удалёнными (`minecraft.versions.pending`) и создаёт
    /// папку только при первом запуске.
    ///
    /// Тогда `family` прочитать неоткуда (у Mojang-версий его нет до
    /// скачивания), но чем лаунчер его заполняет — известно и уже сверено с
    /// его реальными json: [`vanilla::family_of`], то есть `major.minor`.
    /// Поэтому `1.21.11` даёт папку `home/1.21`, а не `home/1.21.11`, и
    /// версию `26.3` — `home/26.3`. Правило то же, что у модовых версий, и
    /// отличать его здесь своим кодом незачем.
    ///
    /// Json уже есть — уважаем написанное в нём поле, а не пересчитываем.
    pub fn vanilla_target_dir(&self, mc_version: &str) -> PathBuf {
        let existing = vanilla::read_version_dir(&self.game_dir.join("versions").join(mc_version));
        let family = existing.as_ref().and_then(|v| v.family.clone()).unwrap_or_else(|| {
            vanilla::family_of(mc_version, LoaderKind::Vanilla)
        });
        match self.separate {
            SeparateDirs::None => self.game_dir.clone(),
            // Режим `version` папку строит по полному имени версии, а не по
            // семейству — здесь `1.21.11` остаётся `1.21.11`.
            SeparateDirs::Version => self.home_dir().join(mc_version),
            SeparateDirs::Family => self.home_dir().join(family),
        }
    }

/// Папки с Java из комплекта лаунчера (`minecraft.jre.dir` и `jre` рядом с
/// exe — там лежит `jre/<arch>/bin/java.exe` и `java-runtime-*`).
pub fn java_roots(&self) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(self.install_dir.join(PROPS_FILE)) {
        if let Some(dir) = vanilla::property_value(&text, PROPS_JRE_DIR_KEY) {
            let dir = PathBuf::from(dir);
            if dir.is_dir() {
                out.push(dir);
            }
        }
    }
    out.push(self.install_dir.join("jre"));
    out
}

    fn preselect(&self) -> PreselectSpec {
        PreselectSpec {
            props_dir: self.install_dir.clone(),
            file_name: PROPS_FILE,
            key: PROPS_VERSION_KEY,
        }
    }
}

/// Папка игры: `versions/` + один из корневых маркеров. Именно этот набор
/// (`assets`, `libraries`, `versions`) называет вики лаунчера, и он же отличает
/// папку игры от подпапки режима — в подпапке этих папок нет.
pub fn is_game_dir(p: &Path) -> bool {
    p.is_dir()
        && p.join("versions").is_dir()
        && (p.join("assets").is_dir() || p.join("libraries").is_dir())
}

/// Папка игры принадлежит Legacy Launcher: подпись `minecraft.gamedir`
/// указывает на неё, а рядом (уровнем выше) — папка установки лаунчера с
/// `LL.exe`/`tl.properties`.
///
/// Нужна детектору, чтобы официальный лаунчер не забрал чужое игровое дерево:
/// в папке игры Legacy тоже лежит `launcher_profiles.json`.
pub fn is_game_dir_of_launcher(p: &Path) -> bool {
    if !is_game_dir(p) {
        return false;
    }
    p.parent().is_some_and(is_install_dir)
        || config_of(p).is_some()
}

/// Найти конфиг лаунчера, которому принадлежит эта папка игры. Идём вверх
/// по родителям (папка установки может быть выше на уровень) и вниз по
/// значению `minecraft.gamedir` из найденных конфигов.
fn config_of(game_dir: &Path) -> Option<LegacyConfig> {
    let mut dir = game_dir.parent().map(|p| p.to_path_buf());
    for _ in 0..3 {
        let Some(d) = dir else { break };
        if let Some(cfg) = LegacyConfig::load(&d) {
            if cfg.game_dir == game_dir {
                return Some(cfg);
            }
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Папка установки лаунчера: `LL.exe` и/или `launcher/bootstrap.jar`.
pub fn is_install_dir(p: &Path) -> bool {
    p.is_dir()
        && (p.join(INSTALL_MARKER_EXE).is_file() || p.join(INSTALL_MARKER_BOOTSTRAP).is_file())
}

/// Имя exe лаунчера в папке установки (для кнопки «Играть»).
pub fn exe_name() -> &'static str {
    INSTALL_MARKER_EXE
}

/// Домашняя папка лаунчеров TLauncher: `%APPDATA%\.tlauncher`.
pub fn launcher_home(roaming: &Path) -> PathBuf {
    roaming.join(".tlauncher")
}

/// Известные места установки Legacy Launcher под домом TLauncher: обычная
/// сборка (`%APPDATA%\.tlauncher\legacy\Minecraft`) и portable (тот же путь
/// внутри папки игры лаунчера — вскрыта на живой машине).
pub fn known_install_dirs(roaming: &Path) -> Vec<PathBuf> {
    let home = launcher_home(roaming);
    let mut out = vec![home.join("legacy").join("Minecraft")];
    // Кто-то мог положить portable-сборку в отдельную папку: `…\.tlauncher\LegacyLauncher`
    // и подобные. Ищем только прямые подпапки дома — это O(1), без обхода диска.
    if let Ok(entries) = std::fs::read_dir(&home) {
        for entry in entries.filter_map(|e| e.ok()) {
            let dir = entry.path();
            if dir.is_dir() {
                out.push(dir);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Цели лаунчера: по одной на папку версии. `path` — подпапка режима, то есть
/// ровно то место, куда лаунчер положит `saves/`.
pub fn enumerate_targets(config: &LegacyConfig) -> Vec<GameTarget> {
    let mut out = Vec::new();
    let versions = config.game_dir.join("versions");
    let entries = std::fs::read_dir(&versions).map(|r| r.collect::<Vec<_>>());
    let mut dirs: Vec<PathBuf> = entries
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Some(v) = vanilla::read_version_dir(&dir) else {
            continue;
        };
        let Ok(target_dir) = config.target_dir(&v.dir_name, v.family.as_deref()) else {
            // FAMILY без поля `family` в json — имя папки придётся угадывать,
            // а угадывать нельзя (моды лягут не туда). Версию пропускаем
            // молча: строгий фильтр важнее полноты списка.
            continue;
        };
        out.push(GameTarget {
            launcher: LauncherKind::Legacy,
            name: v.dir_name,
            path: target_dir,
            minecraft_version: v.mc_version,
            loader: v.loader,
            loader_version: v.loader_version,
        });
    }
    out.sort_by_key(|x| x.name.to_lowercase());
    out
}

/// Подготовка версии: поиск → (закрыть лаунчер → провижининг → пометка
/// `[KK]` → дописать `family`) → предвыбор. Долгая операция, прогресс —
/// событиями `provision-progress`.
///
/// Ванильная карта ничего не провижинит: версия принадлежит лаунчеру, он сам
/// её скачает. Наша работа — указать папку, куда он положит `saves`, и
/// предвыбрать версию, чтобы пользователю не искать её в списке.
pub async fn ensure_version(
    config: &LegacyConfig,
    mc_version: &str,
    loader: LoaderKind,
    loader_version: Option<&str>,
    progress: deploy::Progress<'_>,
) -> Result<vanilla::EnsureResult, InstallError> {
    if mc_version.trim().is_empty() {
        return Err(InstallError::UnknownLoader(
            "неизвестна версия игры для подготовки".to_string(),
        ));
    }
    let loader_version = if loader == LoaderKind::Vanilla {
        None
    } else {
        Some(vanilla::require_exact_loader_version(loader, loader_version)?)
    };

    let dir_name = match loader {
        LoaderKind::Vanilla => mc_version.to_string(),
        _ => {
            let loader_version = loader_version
                .as_deref()
                .ok_or_else(|| InstallError::UnknownLoader("нет версии загрузчика".to_string()))?;
            let existing = vanilla::find_match(&config.game_dir, mc_version, loader, loader_version);
            match existing {
                Some(found) => {
                    let _closed = close_legacy();
                    found.dir_name
                }
                None => {
                    let _closed = close_legacy();
                    let roots = crate::services::java::JavaRoots {
                        mojang: vec![config.game_dir.join("runtime")],
                        bundled: config.java_roots(),
                    };
                    let marked = vanilla::provision(
                        &config.game_dir,
                        mc_version,
                        loader,
                        loader_version,
                        &roots,
                        progress,
                    )
                    .await?;
                    // Без `family` лаунчер в FAMILY-режиме не найдёт папку для saves.
                    vanilla::set_version_family(
                        &config.game_dir,
                        &marked,
                        &vanilla::family_of(mc_version, loader),
                    )?;
                    marked
                }
            }
        }
    };

    let target_dir = if loader == LoaderKind::Vanilla {
        let _closed = close_legacy();
        config.vanilla_target_dir(mc_version)
    } else {
        let version = vanilla::read_version_dir(&config.game_dir.join("versions").join(&dir_name));
        let family = version.as_ref().and_then(|v| v.family.clone());
        config.target_dir(&dir_name, family.as_deref())?
    };
    let mut preselected = false;
    // Лаунчер перезаписывает `tl.properties` при выходе — правим только
    // когда он точно закрыт, иначе молча пропускаем.
    if !legacy_running() {
        preselected = vanilla::set_property(&config.preselect(), &dir_name).unwrap_or(false);
    }
    Ok(vanilla::EnsureResult {
        target: GameTarget {
            launcher: LauncherKind::Legacy,
            name: dir_name.clone(),
            path: target_dir.clone(),
            minecraft_version: mc_version.to_string(),
            loader,
            loader_version,
        },
        mods_empty: vanilla::mods_empty(&target_dir),
        preselected,
    })
}

/// Запущен ли лаунчер: java-процесс, чей exe лежит внутри папки установки
/// лаунчера (LL.exe — 32-битный стаб, он поднимает `jre/<arch>/bin/java.exe`).
/// Игровые `javaw` лежат в рантаймах Mojang и сюда не попадают.
pub(crate) fn is_install_process(exe: Option<&Path>) -> bool {
    exe.is_some_and(|e| {
        // Имя файла, а не весь путь: в сравнении с `"javaw.exe"` полный путь
        // не совпадёт никогда, и лаунчер выглядел бы всегда закрытым — из-за
        // чего `close_launcher` возвращал `false` и фронт никогда не
        // возвращал лаунчер после установки.
        let name = e
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if name != "java.exe" && name != "javaw.exe" && name != "java" {
            return false;
        }
        if launcher_home_from_exe(e).as_os_str().is_empty() {
            return false;
        }
        // Папка игры лежит ВНУТРИ папки установки, и её рантаймы Mojang
        // (`<папка игры>/runtime/java-runtime-*/…`) — те же `javaw.exe`.
        // Отсекаем каталог `runtime`: закрывая лаунчер, убивать идущую игру
        // нельзя, а её JVM подписан тем же именем.
        !e.to_string_lossy()
            .to_lowercase()
            .replace('/', "\\")
            .contains("\\runtime\\")
    })
}

pub(crate) fn legacy_running() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().values().any(|p| is_install_process(p.exe()))
}

/// Папка установки лаунчера по пути его java-процесса: поднимаемся до
/// каталога, где лежит `LL.exe` (у Legacy это `…\.tlauncher\legacy\Minecraft`).
fn launcher_home_from_exe(exe: &Path) -> PathBuf {
    let mut dir = exe.parent().map(|p| p.to_path_buf());
    for _ in 0..8 {
        let Some(d) = dir else { break };
        if is_install_dir(&d) {
            return d;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    PathBuf::new()
}

/// Аккуратно закрывает лаунчер перед правкой версий/конфига. Как `close_tlauncher`:
/// просим процессы завершиться, ждём до ~10 с. `false` — не ошибка.
pub fn close_legacy() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    for _ in 0..40 {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut alive = false;
        for proc in sys.processes().values() {
            if is_install_process(proc.exe()) {
                alive = true;
                proc.kill();
            }
        }
        if !alive {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    !legacy_running()
}

/// «Играть»: выбираем версию и поднимаем лаунчер.
///
/// Аргументов командной строки у Legacy нет. `LL.exe` — заглушка: она
/// ставит `_JAVA_OPTIONS=-DstubLauncher=v1` и передаёт наши аргументы прямо
/// в JVM, который их не знает и падает с «Unrecognized option: --directory /
/// Could not create the Java Virtual Machine». Проверено на живой машине: при
/// этом окно даже не появлялось, то есть кнопка выглядела как неработающая.
///
/// Единственный способ выбрать версию — `login.version` в `tl.properties`,
/// ровно тот же механизм, что и предвыбор после установки. Поэтому пишем его
/// сами (предыдущий выбор пользователя при этом затирается: «Играть» обязана
/// вести на карту, а не на то, что он выбрал раньше) и запускаем лаунчер без
/// аргументов. Файл переписывается, поэтому лаунчер должен быть закрыт, а он
/// однопроцессный — второй экземпляр настройку всё равно не примет.
pub fn launch_legacy(config: &LegacyConfig, version: &str) -> Result<String, InstallError> {
    let _closed = close_legacy();
    let spec = config.preselect();
    if !vanilla::set_property(&spec, version)? {
        return Err(InstallError::LaunchFailed(format!(
            "Legacy Launcher: не найден {}",
            spec.file_name
        )));
    }
    let exe = config.install_dir.join(exe_name());
    std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| InstallError::LaunchFailed(format!("{}: {e}", exe.display())))?;
    Ok(exe.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "живая проверка: печатает версии реального Legacy Launcher"]
    fn live_dumps_legacy_version_slots() {
        let roaming = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA"));
        for dir in known_install_dirs(&roaming) {
            let Some(cfg) = LegacyConfig::load(&dir) else {
                continue;
            };
            println!("--- {}", dir.display());
            for t in enumerate_targets(&cfg) {
                println!(
                    "  слот: mc={} loader={:?} loader_version={:?} dir={} target={}",
                    t.minecraft_version,
                    t.loader,
                    t.loader_version,
                    t.name,
                    t.path.display()
                );
            }
            // Все папки версий на диске — включая те, что целями не стали.
            if let Ok(entries) = std::fs::read_dir(cfg.game_dir.join("versions")) {
                for e in entries.flatten() {
                    if e.path().is_dir() {
                        println!("  папка: {}", e.file_name().to_string_lossy());
                    }
                }
            }
        }
    }

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-legacy-{tag}"))
    }

    fn make_game_dir(root: &Path) -> PathBuf {
        let game = root.join("Minecraft").join("game");
        std::fs::create_dir_all(game.join("versions")).expect("versions");
        std::fs::create_dir_all(game.join("assets")).expect("assets");
        std::fs::create_dir_all(game.join("libraries")).expect("libraries");
        game
    }

    fn make_install_dir(root: &Path) -> PathBuf {
        let inst = root.join("Minecraft");
        std::fs::create_dir_all(inst.join("launcher")).expect("launcher");
        std::fs::write(inst.join(INSTALL_MARKER_EXE), b"fake").expect("exe");
        std::fs::write(inst.join(INSTALL_MARKER_BOOTSTRAP), b"fake").expect("bootstrap");
        inst
    }

    fn write_props(install: &Path, game: &Path, separate: &str) {
        let escaped = game.to_string_lossy().replace('\\', "\\\\").replace(':', "\\:");
        std::fs::write(
            install.join(PROPS_FILE),
            format!(
                "login.version=26.3\nminecraft.gamedir={escaped}\nminecraft.gamedir.separate={separate}\n"
            ),
        )
        .expect("props");
    }

    #[test]
    fn separate_dirs_parses_real_values() {
        assert_eq!(SeparateDirs::parse(Some("none")), SeparateDirs::None);
        assert_eq!(SeparateDirs::parse(Some("family")), SeparateDirs::Family);
        assert_eq!(SeparateDirs::parse(Some("version")), SeparateDirs::Version);
        assert_eq!(SeparateDirs::parse(Some("FAMILY")), SeparateDirs::Family);
        // Неизвестное значение трактуем как `none` (дефолт лаунчера), а не гадаем.
        assert_eq!(SeparateDirs::parse(Some("whatever")), SeparateDirs::None);
        assert_eq!(SeparateDirs::parse(None), SeparateDirs::None);
    }

    #[test]
    fn config_reads_real_tl_properties_shape() {
        let root = temp_root("config");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");

        let cfg = LegacyConfig::load(&install).expect("config");
        assert_eq!(cfg.game_dir, game);
        assert_eq!(cfg.separate, SeparateDirs::Family);
        assert_eq!(cfg.install_dir, install);
    }

    #[test]
    fn target_dir_follows_separate_mode() {
        let root = temp_root("targets");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);

        write_props(&install, &game, "none");
        assert_eq!(
            LegacyConfig::load(&install)
                .expect("cfg")
                .target_dir("NeoForge 1.21.11", Some("NeoForge-1.21"))
                .expect("target"),
            game
        );

        write_props(&install, &game, "family");
        assert_eq!(
            LegacyConfig::load(&install)
                .expect("cfg")
                .target_dir("NeoForge 1.21.11", Some("NeoForge-1.21"))
                .expect("target"),
            game.join("home").join("NeoForge-1.21")
        );

        write_props(&install, &game, "version");
        assert_eq!(
            LegacyConfig::load(&install)
                .expect("cfg")
                .target_dir("NeoForge 1.21.11", Some("NeoForge-1.21"))
                .expect("target"),
            game.join("home").join("NeoForge 1.21.11")
        );
    }

    #[test]
    fn family_mode_without_family_field_is_rejected() {
        let root = temp_root("nofamily");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");
        let cfg = LegacyConfig::load(&install).expect("cfg");
        // Без `family` в json цель невосстановима: гадать нельзя.
        assert!(cfg.target_dir("NeoForge 1.21.11", None).is_err());
    }

    #[test]
    fn enumerate_uses_separate_target_dir_and_skips_incomplete() {
        let root = temp_root("enum");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");
        let cfg = LegacyConfig::load(&install).expect("cfg");

        // Версия как у лаунчера: с полем family.
        let vdir = game.join("versions").join("NeoForge 1.21.11");
        std::fs::create_dir_all(&vdir).expect("vdir");
        std::fs::write(
            vdir.join("NeoForge 1.21.11.json"),
            r#"{"id": "NeoForge 1.21.11", "jar": "1.21.11", "family": "NeoForge-1.21",
                "mainClass": "net.neoforged.fml.startup.Client",
                "libraries": [{"name": "net.neoforged:neoforge:21.1.169"}]}"#,
        )
        .expect("json");
        // Недокачанная версия — только natives/, без json.
        std::fs::create_dir_all(game.join("versions").join("Fabric 26.3").join("natives"))
            .expect("half");

        let targets = enumerate_targets(&cfg);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].launcher, LauncherKind::Legacy);
        assert_eq!(targets[0].name, "NeoForge 1.21.11");
        assert_eq!(targets[0].minecraft_version, "1.21.11");
        assert_eq!(targets[0].loader, LoaderKind::NeoForge);
        assert_eq!(
            targets[0].path,
            game.join("home").join("NeoForge-1.21"),
            "saves кладутся в подпапку семейства"
        );
    }

    #[test]
    fn game_dir_is_distinguished_from_separate_subdir() {
        let root = temp_root("markers");
        let game = make_game_dir(&root);
        assert!(is_game_dir(&game));
        // Подпапка режима: `versions/` там нет — это не папка игры.
        let sub = game.join("home").join("NeoForge-1.21");
        std::fs::create_dir_all(sub.join("saves")).expect("saves");
        assert!(!is_game_dir(&sub));
    }

    #[test]
    fn game_dir_of_launcher_is_recognised_by_install_dir() {
        let root = temp_root("owned");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "none");
        assert!(is_game_dir_of_launcher(&game));
        // Та же папка без папки установки лаунчера — уже не его (кто-то
        // скопировал `versions/` к себе).
        let copy = root.join("copy");
        std::fs::create_dir_all(copy.join("versions")).expect("v");
        std::fs::create_dir_all(copy.join("assets")).expect("a");
        assert!(!is_game_dir_of_launcher(&copy));
    }

    #[test]
    fn install_dir_is_recognized_by_exe_or_bootstrap() {
        let root = temp_root("install");
        let install = make_install_dir(&root);
        assert!(is_install_dir(&install));
        let other = root.join("other");
        std::fs::create_dir_all(&other).expect("dir");
        assert!(!is_install_dir(&other));
    }

    #[test]
    fn family_for_matches_launcher_naming() {
        assert_eq!(vanilla::family_of("26.3", LoaderKind::Vanilla), "26.3");
        assert_eq!(
            vanilla::family_of("1.21.11", LoaderKind::NeoForge),
            "NeoForge-1.21"
        );
        assert_eq!(vanilla::family_of("1.21.11", LoaderKind::Fabric), "Fabric-1.21");
    }

    /// Ванильная версия может быть ещё не скачана — установка обязана
    /// получить папку в любом случае, иначе карта уедет в корень, который в
    /// режимах подпапок лаунчер не смотрит.
    #[test]
    fn vanilla_target_dir_works_before_version_is_downloaded() {
        let root = temp_root("vanilla-target");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");
        let cfg = LegacyConfig::load(&install).expect("config");

        assert!(
            !game.join("versions").join("1.21.11").exists(),
            "условие теста: версии на диске нет",
        );
        assert_eq!(
            cfg.vanilla_target_dir("1.21.11"),
            game.join("home").join("1.21"),
            "FAMILY-папка называется по семейству (major.minor), а не по полной версии",
        );
        assert_ne!(
            cfg.vanilla_target_dir("1.21.11"),
            game.join("home").join("1.21.11"),
            "полное имя версии — типичная ошибка: так папка остаётся невидимой",
        );
    }

    /// Режим `version` папку строит по полному имени версии, а не по
    /// семейству, — там `1.21.11` остаётся `1.21.11`. Путать эти два режима
    /// нельзя.
    #[test]
    fn vanilla_target_dir_uses_full_version_name_in_version_mode() {
        let root = temp_root("vanilla-version-mode");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "version");

        let cfg = LegacyConfig::load(&install).expect("config");
        assert_eq!(
            cfg.vanilla_target_dir("1.21.11"),
            game.join("home").join("1.21.11"),
        );
    }

    /// Семейство — то же, что считает лаунчер для модовых версий: отдельная
    /// реализация правила разъехалась бы с общей при следующей правке.
    #[test]
    fn vanilla_target_dir_reuses_launcher_family_rule() {
        for (mc, family) in [("1.21.11", "1.21"), ("1.8.9", "1.8"), ("26.3", "26.3")] {
            let root = temp_root(&format!("vanilla-family-{mc}"));
            let install = make_install_dir(&root);
            let game = make_game_dir(&root);
            write_props(&install, &game, "family");

            let cfg = LegacyConfig::load(&install).expect("config");
            assert_eq!(
                cfg.vanilla_target_dir(mc),
                game.join("home").join(family),
                "{mc} -> {family}",
            );
            assert_eq!(
                family,
                vanilla::family_of(mc, LoaderKind::Vanilla),
                "правило семейства должно быть тем же, что и у лаунчера",
            );
        }
    }

    #[test]
    fn vanilla_target_dir_follows_every_separate_mode() {
        for (separate, expected) in [
            ("none", ""),
            ("family", "home/1.21"),
            ("version", "home/1.21.11"),
        ] {
            let root = temp_root(&format!("vanilla-mode-{separate}"));
            let install = make_install_dir(&root);
            let game = make_game_dir(&root);
            write_props(&install, &game, separate);
            let cfg = LegacyConfig::load(&install).expect("config");
            assert_eq!(cfg.vanilla_target_dir("1.21.11"), game.join(expected), "{separate}");
        }
    }

    /// Уже скачанную версию мы не переигрываем: если лаунчер или установщик
    /// задали в json своё `family`, папка должна быть той же.
    #[test]
    fn vanilla_target_dir_respects_existing_family_field() {
        let root = temp_root("vanilla-family");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");
        let dir = game.join("versions").join("1.21.11");
        std::fs::create_dir_all(&dir).expect("versions dir");
        std::fs::write(
            dir.join("1.21.11.json"),
            r#"{"id":"1.21.11","family":"my-own","jar":"1.21.11"}"#,
        )
        .expect("json");

        let cfg = LegacyConfig::load(&install).expect("config");
        assert_eq!(cfg.vanilla_target_dir("1.21.11"), game.join("home").join("my-own"));
    }

    /// Ванильная карта не должна требовать точной версии загрузчика: её нет
    /// вовсе, и требование превращало бы установку в ошибку.
    #[test]
    fn vanilla_requires_no_loader_version() {
        let err = vanilla::require_exact_loader_version(LoaderKind::Vanilla, None)
            .expect_err("ванилле версия загрузчика не нужна");
        assert!(matches!(err, InstallError::UnknownLoader(_)));
    }

    /// Наружу отдаётся `exe_path`, и фронтенд честно передаёт его сюда как есть.
    /// Раньше это давало «не найден tl.properties»: папка установки и путь к
    /// `LL.exe` отличаются одним компонентом, и ошибка выглядела как «нет
    /// конфига», хотя конфиг рядом.
    #[test]
    fn load_accepts_exe_path_as_well_as_install_dir() {
        let root = temp_root("load-exe");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");

        let from_dir = LegacyConfig::load(&install).expect("из папки");
        let from_exe = LegacyConfig::load(&install.join(INSTALL_MARKER_EXE)).expect("из exe");
        assert_eq!(from_exe.game_dir, from_dir.game_dir);
        assert_eq!(from_exe.install_dir, install);
    }

    /// `LL.exe` может быть ещё не скачан — тогда путь не существует вовсе, и
    /// проверять надо `is_dir`, а не `is_file`: иначе мы решили бы, что
    /// несуществующий exe это папка, и потеряли конфиг.
    #[test]
    fn load_accepts_missing_exe_path() {
        let root = temp_root("load-missing-exe");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "version");

        std::fs::remove_file(install.join(INSTALL_MARKER_EXE)).expect("убираем exe");
        assert!(
            !install.join(INSTALL_MARKER_EXE).exists(),
            "условие теста: exe отсутствует",
        );
        let cfg = LegacyConfig::load(&install.join(INSTALL_MARKER_EXE)).expect("из несуществующего exe");
        assert_eq!(cfg.game_dir, game);
    }

    /// «Играть» пишет `login.version` — единственный способ выбрать версию у
    /// Legacy, своего CLI у него нет.
    #[test]
    fn launch_selects_version_by_property() {
        let root = temp_root("launch-select");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");
        let cfg = LegacyConfig::load(&install).expect("config");

        // `LL.exe` — заглушка, настоящий запуск в тесте не нужен: важно, что
        // версия записана ДО попытки запуска, иначе лаунчер поднимется со
        // старым выбором.
        let _ = launch_legacy(&cfg, "1.21.11");

        let props = std::fs::read_to_string(install.join(PROPS_FILE)).expect("props");
        assert!(
            props.contains("login.version=1.21.11"),
            "версия должна быть выбрана в конфиге: {props}",
        );
    }

    /// Выбор пользователя в лаунчере не должен уводить «Играть» с карты: кнопка
    /// переписывает `login.version` на версию цели.
    #[test]
    fn launch_overwrites_previously_selected_version() {
        let root = temp_root("launch-overwrite");
        let install = make_install_dir(&root);
        let game = make_game_dir(&root);
        write_props(&install, &game, "family");
        let cfg = LegacyConfig::load(&install).expect("config");
        std::fs::write(
            install.join(PROPS_FILE),
            "login.version=26.3\nminecraft.gamedir=C\\:\\\\g\nminecraft.gamedir.separate=family\n",
        )
        .expect("props");

        let _ = launch_legacy(&cfg, "NeoForge 1.21.11");

        let props = std::fs::read_to_string(install.join(PROPS_FILE)).expect("props");
        assert!(props.contains("login.version=NeoForge 1.21.11"), "{props}");
        assert!(!props.contains("login.version=26.3"), "старый выбор должен уйти: {props}");
    }

    /// Живая проверка на машине разработчика: версия-сирота без `.jar`
    /// (только json с `jar` и `family`) должна попасть в цели с папкой
    /// `home/<family>`. `#[ignore]` — набор установленных версий у каждого
    /// человека свой.
    #[test]
    #[ignore = "живая проверка целей Legacy, зависит от машины"]
    fn live_lists_vanilla_alias_version() {
        let roaming = dirs::config_dir().expect("%APPDATA%");
        let install = known_install_dirs(&roaming)
            .into_iter()
            .find(|d| is_install_dir(d))
            .expect("папка установки Legacy");
        let cfg = LegacyConfig::load(&install).expect("tl.properties");
        println!("game_dir={} separate={:?}", cfg.game_dir.display(), cfg.separate);
        let targets = enumerate_targets(&cfg);
        for t in &targets {
            println!("  {} -> {}", t.name, t.path.display());
        }
        let alias = targets
            .iter()
            .find(|t| t.name == "DeepMine 1.21.11")
            .expect("версия-сирота должна быть в целях");
        assert_eq!(
            alias.path,
            cfg.game_dir.join("home").join("DeepMine 1.21.11"),
            "в FAMILY-режиме папка берётся из поля family",
        );
    }

    /// Живая проверка на машине разработчика: ванильная версия `1.21.11` на
    /// диске отсутствует (Legacy держит её удалённой), и папка всё равно должна
    /// вычислиться — иначе карта уедет в корень, который лаунчер не смотрит.
    #[test]
    #[ignore = "живая проверка на машине разработчика"]
    fn live_vanilla_target_for_map_version() {
        let roaming = dirs::config_dir().expect("%APPDATA%");
        let install = known_install_dirs(&roaming)
            .into_iter()
            .find(|d| is_install_dir(d))
            .expect("папка установки Legacy");
        let cfg = LegacyConfig::load(&install).expect("tl.properties");
        println!("separate={:?}", cfg.separate);
        println!("версия 1.21.11 на диске: {}", cfg.game_dir.join("versions").join("1.21.11").exists());
        println!("цель для 1.21.11: {}", cfg.vanilla_target_dir("1.21.11").display());
        println!("цель для 26.3:    {}", cfg.vanilla_target_dir("26.3").display());
        println!("цель для 1.8.9:   {}", cfg.vanilla_target_dir("1.8.9").display());
    }

    /// Живая проверка ровно того вызова, который падал в приложении:
    /// фронтенд отдаёт `exe_path`, а не папку установки.
    #[test]
    #[ignore = "живая проверка на машине разработчика"]
    fn live_loads_config_from_exe_path() {
        let roaming = dirs::config_dir().expect("%APPDATA%");
        let install = known_install_dirs(&roaming)
            .into_iter()
            .find(|d| is_install_dir(d))
            .expect("папка установки Legacy");
        let exe = install.join(exe_name());
        println!("exe существует: {}", exe.is_file());
        let cfg = LegacyConfig::load(&exe).expect("конфиг по пути к exe");
        println!("game_dir={}", cfg.game_dir.display());
        println!("separate={:?}", cfg.separate);
        println!("цель 1.21.11={}", cfg.vanilla_target_dir("1.21.11").display());
    }

/// Регрессия: сверялось имя файла с полным путём, из-за чего лаунчер всегда
    /// считался закрытым — `close_launcher` отдавал `false`, а фронт по этому
    /// флагу не возвращал лаунчер после установки.
    #[test]
    fn install_process_matches_javaw_under_install_dir() {
        let root = temp_root("install-process");
        let install = make_install_dir(&root);
        let jre = install.join("jre").join("x64").join("bin");

        assert!(is_install_process(Some(&jre.join("javaw.exe"))));
        assert!(is_install_process(Some(&jre.join("java.exe"))));
        // Реальный путь, как его отдаёт sysinfo.
        assert!(is_install_process(Some(
            &install.join("jre").join("x64").join("bin").join("javaw.exe")
        )));
    }

    #[test]
    fn install_process_ignores_game_javaw_and_foreign_exes() {
        let root = temp_root("install-process-foreign");
        let install = make_install_dir(&root);

        // Рантайм Mojang лежит внутри папки установки (папка игры — вложена в
        // неё), но это игра, а не лаунчер: закрывать её нельзя.
        assert!(!is_install_process(Some(
            &install
                .join("game")
                .join("runtime")
                .join("java-runtime-delta")
                .join("windows-x64")
                .join("java-runtime-delta")
                .join("bin")
                .join("javaw.exe")
        )));
        // Совпадение по имени есть, а папки установки — нет.
        let other = root.join("other").join("Minecraft");
        std::fs::create_dir_all(other.join("jre").join("bin")).expect("dir");
        assert!(!is_install_process(Some(&other.join("jre").join("bin").join("javaw.exe"))));
        assert!(!is_install_process(Some(&install.join(INSTALL_MARKER_EXE))));
        assert!(!is_install_process(None));
    }

    /// Живая проверка детекта запущенного Legacy на машине разработчика.
    /// Ответ `false` при открытом окне означает, что `close_launcher` не
    /// увидит лаунчер, фронт не запомнит флаг и не вернёт его после установки.
    #[test]
    #[ignore = "живая проверка детекта процессов"]
    fn live_detects_running_legacy() {
        println!("legacy_running(): {}", legacy_running());
        println!(
            "close_launcher(Legacy): {}",
            crate::services::detect::close_launcher(LauncherKind::Legacy)
        );
        println!("после close legacy_running(): {}", legacy_running());
    }
}