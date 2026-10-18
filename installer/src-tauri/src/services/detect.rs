//! Детект лаунчеров и их инстансов/профилей (§6.14 ТЗ, §5–6 дизайн-дока).
//!
//! Принципы (согласованы с пользователем):
//! - никакого full-disk scan — только точечные O(1)-проверки, параллельно;
//! - слои: сохранённые ручные пути → реестр протоколов/ассоциаций →
//!   запущенные процессы → известные корни (+портативка рядом с exe) → PATH;
//! - ручной выбор — последний fallback, один раз, с запоминанием в store;
//! - кандидаты валидируются маркерами, скоринг только для сортировки.

use crate::adapters::tlauncher::is_tlauncher_data_dir;
use crate::domain::{GameTarget, LauncherInfo, LauncherKind, LoaderKind};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Итог детекта: найденные лаунчеры + флаги запущенных приложений
/// (запущенные перед записью автоматически закрываются; если не
/// закрылись — установка идёт всё равно, профиль подхватится после
/// ручного перезапуска).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Detection {
    pub launchers: Vec<LauncherInfo>,
    /// Какие лаунчеры сейчас запущены. Список, а не набор булевых флагов:
    /// лаунчеров много, а подсказка про ошибку интересует все закрываемые.
    pub running: Vec<LauncherKind>,
}

/// Полный детект. `manual_roots` — ручные пути из store (слой 0, они же
/// кандидаты в ручные папки). `known_roots` — ранее найденные exe/корни
/// (запоминает фронтенд): так портативка находится и без запуска.
pub fn detect_launchers(manual_roots: &[PathBuf], known_roots: &[PathBuf]) -> Detection {
    let processes = running_launcher_processes();

    let mut launchers: Vec<LauncherInfo> = Vec::new();
    launchers.extend(detect_mmc(manual_roots, known_roots, &processes, &PRISM));
    launchers.extend(detect_mmc(manual_roots, known_roots, &processes, &PINECONE));
    launchers.extend(detect_mmc(manual_roots, known_roots, &processes, &POLYMC));
    launchers.extend(detect_mmc(manual_roots, known_roots, &processes, &MULTIMC));
    launchers.extend(detect_modrinth(manual_roots, known_roots, &processes));
    launchers.extend(detect_tlauncher(manual_roots, known_roots));
    let legacy = detect_legacy(manual_roots, known_roots);
    let official = detect_official(manual_roots, known_roots);
    launchers.extend(legacy.iter().cloned());
    launchers.extend(official.iter().cloned());

// Папки, которые уже заняты карточками Legacy/официального лаунчера:
    // ручной путь в них не должен превращаться в ещё одну карточку «вручную».
    let claimed: Vec<PathBuf> = legacy
        .iter()
        .map(|l| l.data_dir.clone())
        .chain(official.iter().map(|l| l.data_dir.clone()))
        .collect();

    for root in manual_roots {
        if is_any_mmc_data_dir(root) || is_modrinth_data_dir(root) {
            continue; // уже покрыты выше
        }
        // `.minecraft` общий у TLauncher и официального лаунчера, а папка игры
        // Legacy лежит внутри `.tlauncher`. Владелец один: кто специфичнее по
        // маркерам, тот и забирает корень (иначе одна папка дала бы две
        // карточки-дубля).
        if is_tlauncher_data_dir(root)
            || is_official_data_dir(root)
            || claimed.iter().any(|d| d == root)
        {
            continue;
        }
if let Some(target) = manual_target(root) {
            launchers.push(LauncherInfo {
                kind: LauncherKind::Manual,
                display_name: format!("Папка: {}", target.name),
                exe_path: None,
                data_dir: root.clone(),
                portable: false,
                targets: vec![target],
                app_found: true,
            });
        }
    }

    let extra: [(LauncherKind, bool); 2] = [
        (
            LauncherKind::Tlauncher,
            crate::adapters::tlauncher::tlauncher_running(),
        ),
        (
            LauncherKind::Legacy,
            crate::adapters::legacy::legacy_running(),
        ),
    ];
    let mut running: Vec<LauncherKind> = processes
        .iter()
        .map(|p| p.kind)
        .chain(
            extra
                .into_iter()
                .filter(|(_, is_running)| *is_running)
                .map(|(kind, _)| kind),
        )
        .collect();
    running.sort_by_key(|k| format!("{k:?}"));
    running.dedup();

    Detection {
        launchers,
        running,
    }
}

/// Кандидат в корень данных: откуда exe (для «Играть»), сам корень и флаг
/// портативности (данные рядом с exe, а не в стандартном месте).
struct DataRoot {
    exe: Option<PathBuf>,
    dir: PathBuf,
    portable: bool,
}

/// Параметры одного лаунчера Prism-семейства: формат инстансов
/// (`instances/*/instance.cfg + mmc-pack.json`) и CLI (`--launch <id>`)
/// общие, имена exe/конфигов/папок данных — разные.
#[derive(Clone, Copy)]
struct MmcFamily {
    kind: LauncherKind,
    /// Префиксы имён exe в нижнем регистре: совпадение по `starts_with`.
    exe_prefixes: &'static [&'static str],
    /// Файлы-маркеры корня данных (помимо `instances/`).
    cfg_markers: &'static [&'static str],
    /// Папки данных в `%APPDATA%` (Windows) / `data_dir` (Linux).
    /// Пусто — штатных путей нет, только портативка (как у MultiMC).
    data_dirs: &'static [&'static str],
    display: &'static str,
}

const PRISM: MmcFamily = MmcFamily {
    kind: LauncherKind::Prism,
    exe_prefixes: &["prismlauncher"],
    cfg_markers: &["prismlauncher.cfg"],
    data_dirs: &["PrismLauncher"],
    display: "Prism Launcher",
};

// Одна карточка «Pinecone Launcher»: бинарь до сих пор называется
// `elyprismlauncher` (см. `Launcher_APP_BINARY_NAME` в их CMakeLists),
// данные — `%APPDATA%\ElyPrismLauncher`; `pinecone*` — задел на будущее
// переименование после ребренда из ElyPrism.
const PINECONE: MmcFamily = MmcFamily {
    kind: LauncherKind::Pinecone,
    exe_prefixes: &["elyprismlauncher", "pineconemc", "pinecone"],
    cfg_markers: &["elyprismlauncher.cfg", "pineconemc.cfg"],
    data_dirs: &["ElyPrismLauncher", "PineconeMC"],
    display: "Pinecone Launcher",
};

const POLYMC: MmcFamily = MmcFamily {
    kind: LauncherKind::Polymc,
    exe_prefixes: &["polymc"],
    cfg_markers: &["polymc.cfg"],
    data_dirs: &["PolyMC"],
    display: "PolyMC",
};

// Штатного установщика нет (только portable): `data_dirs` проверяем всё
// равно — O(1) и задел, если появится неportable-вариант.
const MULTIMC: MmcFamily = MmcFamily {
    kind: LauncherKind::Multimc,
    exe_prefixes: &["multimc"],
    cfg_markers: &["multimc.cfg"],
    data_dirs: &["MultiMC"],
    display: "MultiMC",
};

fn mmc_family(kind: LauncherKind) -> &'static MmcFamily {
    match kind {
        LauncherKind::Prism => &PRISM,
        LauncherKind::Pinecone => &PINECONE,
        LauncherKind::Polymc => &POLYMC,
        LauncherKind::Multimc => &MULTIMC,
        LauncherKind::Modrinth | LauncherKind::Manual | LauncherKind::Tlauncher | LauncherKind::Legacy | LauncherKind::Official => {
            panic!("not an MMC-family launcher")
        }
    }
}

/// Сбор корней Prism-семейства: портативки рядом с каждым найденным exe
/// (слой processes/реестр/известные/PATH/ярлыки/запомненные/ручные) +
/// стандартный корень. Одинаковые корни схлопываются — разные остаются
/// разными карточками.
fn detect_mmc(
    manual_roots: &[PathBuf],
    known_roots: &[PathBuf],
    processes: &[RunningLauncher],
    fam: &'static MmcFamily,
) -> Vec<LauncherInfo> {
    let mut exes: Vec<PathBuf> = Vec::new();
    exes.extend(manual_roots.iter().filter(|p| is_mmc_exe(p, fam)).cloned());
    exes.extend(known_roots.iter().filter(|p| is_mmc_exe(p, fam)).cloned());
    exes.extend(
        processes
            .iter()
            .filter(|p| p.kind == fam.kind)
            .map(|p| p.exe.clone()),
    );
    // Реестр протоколов разбираем только у Prism: у Modrinth/TLauncher этого
    // слоя тоже нет, форкам отдельные схемы не гарантированы.
    if fam.kind == LauncherKind::Prism {
        if let Some(exe) = exe_from_protocol() {
            if is_mmc_exe(&exe, fam) {
                exes.push(exe);
            }
        }
    }
    exes.extend(known_mmc_exe(fam));
    exes.extend(start_menu_exes().into_iter().filter(|p| is_mmc_exe(p, fam)));
    exes.extend(exes_from_path_search(fam));

    let mut roots: Vec<DataRoot> = Vec::new();
    for exe in &exes {
        if let Some(dir) = data_root_next_to_exe(exe, fam.kind) {
            roots.push(DataRoot {
                exe: Some(exe.clone()),
                dir,
                portable: true,
            });
        }
    }
    if let Some(dir) = known_mmc_data_root(fam) {
        let exe = exes.into_iter().next();
        roots.push(DataRoot {
            exe,
            dir,
            portable: false,
        });
    }
    for root in manual_roots.iter().chain(known_roots.iter()) {
        if is_mmc_data_dir(root, fam) && mmc_claim(root, fam.kind) {
            roots.push(DataRoot {
                exe: None,
                dir: root.clone(),
                portable: false,
            });
        }
    }

dedup_roots(roots)
        .into_iter()
        .map(|r| {
            let exe = r.exe.or_else(|| mmc_exe_next_to_data(&r.dir, fam));
            LauncherInfo {
                kind: fam.kind,
                display_name: if r.portable {
                    format!("{} (портативный)", fam.display)
                } else {
                    fam.display.to_string()
                },
                app_found: app_installed(fam.kind, exe.as_deref()),
                exe_path: exe,
                targets: enumerate_mmc_targets(&r.dir, fam.kind),
                data_dir: r.dir,
                portable: r.portable,
            }
        })
        .collect()
}

/// Сбор корней Modrinth App — симметрично Prism (портативный режим тоже
/// возможен: данные рядом с exe проверяются теми же маркерами).
fn detect_modrinth(
    manual_roots: &[PathBuf],
    known_roots: &[PathBuf],
    processes: &[RunningLauncher],
) -> Vec<LauncherInfo> {
    let mut exes: Vec<PathBuf> = Vec::new();
    exes.extend(manual_roots.iter().filter(|p| is_modrinth_exe(p)).cloned());
    exes.extend(known_roots.iter().filter(|p| is_modrinth_exe(p)).cloned());
    exes.extend(
        processes
            .iter()
            .filter(|p| p.kind == LauncherKind::Modrinth)
            .map(|p| p.exe.clone()),
    );
    exes.extend(known_modrinth_exe());
    exes.extend(
        start_menu_exes()
            .into_iter()
            .filter(|p| is_modrinth_exe(p)),
    );

    let mut roots: Vec<DataRoot> = Vec::new();
    for exe in &exes {
        if let Some(dir) = data_root_next_to_exe(exe, LauncherKind::Modrinth) {
            roots.push(DataRoot {
                exe: Some(exe.clone()),
                dir,
                portable: true,
            });
        }
    }
    if let Some(dir) = known_modrinth_data_root() {
        let exe = exes.into_iter().next();
        roots.push(DataRoot {
            exe,
            dir,
            portable: false,
        });
    }
    for root in manual_roots.iter().chain(known_roots.iter()) {
        if is_modrinth_data_dir(root) {
            roots.push(DataRoot {
                exe: None,
                dir: root.clone(),
                portable: false,
            });
        }
    }

dedup_roots(roots)
        .into_iter()
        .map(|r| {
            // Портативная сборка Modrinth: exe рядом с папкой данных.
            let exe = r.exe.or_else(|| {
                std::fs::read_dir(&r.dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .map(|e| e.path())
                            .find(|p| is_modrinth_exe(p))
                    })
                    .ok()
                    .flatten()
            });
            LauncherInfo {
                kind: LauncherKind::Modrinth,
                display_name: if r.portable {
                    "Modrinth App (портативный)".to_string()
                } else {
                    "Modrinth App".to_string()
                },
                app_found: app_installed(LauncherKind::Modrinth, exe.as_deref()),
                exe_path: exe,
                targets: read_modrinth_targets(&r.dir).unwrap_or_default(),
                data_dir: r.dir,
                portable: r.portable,
            }
        })
        .collect()
}

/// Кажется ли приложение установленным: exe на месте, процесс запущен или
/// есть запись в реестре установленных программ.
///
/// Реестр — единственный честный признак «стоял и удалён»: папка с данными
/// после удаления остаётся, и по ней отличить установленное приложение от
/// брошенной папки нельзя. Но у портативных сборок (MultiMC, TLauncher
/// portable) записи в реестре нет никогда — поэтому отсутствие записи сама
/// по себе ничего не доказывает, и такие лаунчеры мы не прячем.
#[cfg(windows)]
fn registered_in_windows(fragments: &[&str]) -> bool {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    use winreg::RegKey;
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let Ok(uninstall) = RegKey::predef(root)
            .open_subkey_with_flags("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall", KEY_READ | KEY_WOW64_64KEY)
        else {
            continue;
        };
        for name in uninstall.enum_keys().flatten() {
            let Ok(sub) = uninstall.open_subkey(name) else {
                continue;
            };
            let display: String = sub.get_value("DisplayName").unwrap_or_default();
            let lower = display.to_lowercase();
            if fragments.iter().any(|f| lower.contains(&f.to_lowercase())) {
                return true;
            }
        }
    }
    false
}

#[cfg(not(windows))]
fn registered_in_windows(_fragments: &[&str]) -> bool {
    false
}

/// Признак установленного приложения для карточки лаунчера.
fn app_installed(kind: LauncherKind, exe: Option<&Path>) -> bool {
    if exe.is_some_and(|p| p.is_file()) {
        return true;
    }
    let fragments: &[&str] = match kind {
        LauncherKind::Prism => &["prism launcher"],
        LauncherKind::Pinecone => &["pinecone", "elyprism"],
        LauncherKind::Polymc => &["polymc"],
        LauncherKind::Multimc => &["multimc"],
        LauncherKind::Modrinth => &["modrinth"],
        LauncherKind::Tlauncher => &["tlauncher"],
        LauncherKind::Legacy => &["legacy launcher"],
        LauncherKind::Official => &["minecraft launcher"],
        LauncherKind::Manual => &[],
    };
    !fragments.is_empty() && registered_in_windows(fragments)
}

/// Сбор корней TLauncher: дефолтный `.minecraft` + `minecraft.gamedir`
/// из конфига + ручные/известные корни и exe. Инстансов нет — цели
/// перечисляются из `versions/` при сборке карточки.
fn detect_tlauncher(manual_roots: &[PathBuf], known_roots: &[PathBuf]) -> Vec<LauncherInfo> {
    use crate::adapters::tlauncher as tl;
    let mut roots: Vec<DataRoot> = Vec::new();
    let mut exes: Vec<PathBuf> = Vec::new();
    exes.extend(manual_roots.iter().filter(|p| is_tlauncher_exe(p)).cloned());
    exes.extend(known_roots.iter().filter(|p| is_tlauncher_exe(p)).cloned());
    exes.extend(start_menu_exes().into_iter().filter(|p| is_tlauncher_exe(p)));
    for exe in &exes {
        // Exe обычно лежит прямо в корне данных (живой кейс) — проверяем
        // саму папку exe, а не только «рядом» (там логика Prism/Modrinth).
        if let Some(dir) = exe.parent() {
            if tl::is_tlauncher_data_dir(dir) {
                roots.push(DataRoot {
                    exe: Some(exe.clone()),
                    dir: dir.to_path_buf(),
                    portable: false,
                });
            }
        }
        if let Some(dir) = data_root_next_to_exe(exe, LauncherKind::Tlauncher) {
            roots.push(DataRoot {
                exe: Some(exe.clone()),
                dir,
                portable: true,
            });
        }
    }
    if let Some(dir) = known_tlauncher_data_root() {
        let exe = exes.into_iter().next().or_else(|| tl::find_tlauncher_exe(&dir));
        roots.push(DataRoot {
            exe,
            dir,
            portable: false,
        });
    }
    for root in manual_roots.iter().chain(known_roots.iter()) {
        if tl::is_tlauncher_data_dir(root) {
            roots.push(DataRoot {
                exe: tl::find_tlauncher_exe(root),
                dir: root.clone(),
                portable: false,
            });
        }
    }

dedup_roots(roots)
        .into_iter()
        .map(|r| {
            let exe = r.exe.or_else(|| tl::find_tlauncher_exe(&r.dir));
            LauncherInfo {
                kind: LauncherKind::Tlauncher,
                display_name: if r.portable {
                    "TLauncher (портативный)".to_string()
                } else {
                    "TLauncher".to_string()
                },
                app_found: exe.is_some(),
                exe_path: exe,
                targets: tl::enumerate_targets(&r.dir),
                data_dir: r.dir,
                portable: r.portable,
            }
        })
        .collect()
}

/// Сбор корней Legacy Launcher: папки установки лаунчера (где лежит
/// `LL.exe` и `tl.properties`), из них берётся `minecraft.gamedir` — папка
/// игры. Инстансов нет: цели перечисляются из `versions/` при сборке карточки,
/// а `path` цели — подпапка выбранного в лаунчере режима.
fn detect_legacy(manual_roots: &[PathBuf], known_roots: &[PathBuf]) -> Vec<LauncherInfo> {
    use crate::adapters::legacy as lg;
    let mut install_dirs: Vec<PathBuf> = Vec::new();
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        install_dirs.extend(lg::known_install_dirs(&roaming));
    }
    install_dirs.extend(
        manual_roots
            .iter()
            .chain(known_roots.iter())
            .filter(|p| lg::is_install_dir(p))
            .cloned(),
    );
let mut seen: std::collections::BTreeSet<PathBuf> = Default::default();

    let mut out: Vec<LauncherInfo> = Vec::new();
    for install in install_dirs {
        let Some(config) = lg::LegacyConfig::load(&install) else {
            continue; // конфига нет/битый — лаунчер сюда не настроен
        };
        let key = install.canonicalize().unwrap_or_else(|_| install.clone());
        if !seen.insert(key) {
            continue;
        }
        let exe = install.join(lg::exe_name());
        out.push(LauncherInfo {
            kind: LauncherKind::Legacy,
            display_name: "Legacy Launcher".to_string(),
            app_found: exe.is_file() || app_installed(LauncherKind::Legacy, Some(&exe)),
            exe_path: Some(exe),
            targets: lg::enumerate_targets(&config),
            data_dir: config.game_dir,
            portable: false,
        });
    }
    out
}

/// Кому принадлежит папка игровых данных. Маркеры специфичны в этом
/// порядке: TLauncher (его exe лежит прямо в папке) → Legacy (папка
/// установки лаунчера с `tl.properties`/`LL.exe`) → официальный лаунчер.
/// Одна папка отдаёт ровно одну карточку, иначе пользователь видит
/// «Minecraft Launcher», у которого на самом деле чужие данные.
fn data_dir_owner(dir: &Path) -> Option<LauncherKind> {
    if is_tlauncher_data_dir(dir) {
        return Some(LauncherKind::Tlauncher);
    }
    if crate::adapters::legacy::is_game_dir_of_launcher(dir) {
        return Some(LauncherKind::Legacy);
    }
    if is_official_data_dir(dir) {
        return Some(LauncherKind::Official);
    }
    None
}

/// Сбор корней официального Minecraft Launcher: `%APPDATA%\.minecraft`.
/// Store-сборка лежит в `WindowsApps` и для прямого запуска закрыта, поэтому
/// exe тут нужен только для старой Standalone-сборки; Store поднимаем по AUMID.
fn detect_official(manual_roots: &[PathBuf], known_roots: &[PathBuf]) -> Vec<LauncherInfo> {
    let mut roots: Vec<DataRoot> = Vec::new();
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        let def = crate::adapters::official::default_data_dir(&roaming);
        if is_official_data_dir(&def) {
            roots.push(DataRoot {
                exe: official_exe(),
                dir: def,
                portable: false,
            });
        }
    }
    for root in manual_roots.iter().chain(known_roots.iter()) {
        if is_official_data_dir(root) && data_dir_owner(root) == Some(LauncherKind::Official) {
            roots.push(DataRoot {
                exe: official_exe(),
                dir: root.clone(),
                portable: false,
            });
        }
    }
    dedup_roots(roots)
        .into_iter()
        .map(|r| {
            let app_found = r.exe.is_some() || crate::adapters::official::store_app_installed();
            LauncherInfo {
                kind: LauncherKind::Official,
                display_name: "Minecraft Launcher".to_string(),
                exe_path: r.exe,
                targets: crate::adapters::official::enumerate_targets(&r.dir),
                data_dir: r.dir,
                portable: r.portable,
                app_found,
            }
        })
        .collect()
}

/// Корень данных официального лаунчера (`%APPDATA%\.minecraft`).
fn is_official_data_dir(p: &Path) -> bool {
    crate::adapters::official::is_data_dir(p)
}

/// Exe Standalone-сборки официального лаунчера. Store-сборки здесь нет и
/// быть не может: она в закрытой `WindowsApps`, её поднимают по AUMID.
fn official_exe() -> Option<PathBuf> {
    [
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        dirs::data_local_dir().map(|p| p.join("Programs")),
    ]
    .into_iter()
    .flatten()
    .map(|base| base.join("Minecraft Launcher").join("MinecraftLauncher.exe"))
    .find(|c| c.is_file())
}

/// Exe TLauncher (`TLauncher*.exe`, регистронезависимо).
fn is_tlauncher_exe(p: &Path) -> bool {
    p.is_file()
        && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        && p.file_stem()
            .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("tlauncher"))
}

/// Корень данных TLauncher: дефолт `%APPDATA%/.minecraft` (валидирован
/// маркерами — чужой ванильный каталог не подхватим) либо `minecraft.gamedir`
/// из конфига лаунчера.
fn known_tlauncher_data_root() -> Option<PathBuf> {
    // 1. Явный game dir из конфига запущенного/настроенного лаунчера.
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        if let Some(dir) = crate::adapters::tlauncher::game_dir_from_props(&roaming) {
            return Some(dir);
        }
        // 2. Дефолт.
        let def = roaming.join(".minecraft");
        if crate::adapters::tlauncher::is_tlauncher_data_dir(&def) {
            return Some(def);
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = dirs::home_dir() {
            let def = home.join(".minecraft");
            if crate::adapters::tlauncher::is_tlauncher_data_dir(&def) {
                return Some(def);
            }
        }
    }
    None
}

/// Схлопывание дублей по каноническому корню. Первое вхождение побеждает.
fn dedup_roots(roots: Vec<DataRoot>) -> Vec<DataRoot> {
    let mut seen: std::collections::BTreeSet<PathBuf> = Default::default();
    roots
        .into_iter()
        .filter(|r| {
            let key = r.dir.canonicalize().unwrap_or_else(|_| r.dir.clone());
            seen.insert(key)
        })
        .collect()
}

pub(crate) struct RunningLauncher {
    pub(crate) kind: LauncherKind,
    exe: PathBuf,
}

/// Пути exe запущенных лаунчеров. Заодно ловит портативку на любом диске.
pub(crate) fn running_launcher_processes() -> Vec<RunningLauncher> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut out = Vec::new();
    for proc in sys.processes().values() {
        if let Some(kind) = process_kind(proc) {
            if let Some(exe) = proc.exe().map(|p| p.to_path_buf()) {
                out.push(RunningLauncher { kind, exe });
            }
        }
    }
    out
}

/// Вид процесса по имени exe: Prism-семейство — по префиксам, Modrinth — по
/// именам, TLauncher и Legacy Launcher — java-процессы (их лаунчеры написаны
/// на Java), официальный лаунчер — `Minecraft.exe` из Store-пакета.
/// Единая точка для детекта запущенных и закрытия.
///
/// Про официальный лаунчер: имя `Minecraft.exe` само по себе ничего не значит
/// (так называется и Bedrock-игра), поэтому решает только пакет в пути —
/// `Microsoft.4297127D64EC6`. Игровые процессы там тоже `Minecraft.exe`, но
/// из другого пакета, и в выборку они не попадают.
fn process_kind(proc: &sysinfo::Process) -> Option<LauncherKind> {
    let name = proc.name().to_string_lossy().to_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    if let Some(fam) = [&PRISM, &PINECONE, &POLYMC, &MULTIMC]
        .into_iter()
        .find(|fam| fam.exe_prefixes.iter().any(|p| stem == *p))
    {
        return Some(fam.kind);
    }
    if name == "modrinth app.exe" || name == "modrinthapp.exe" || name == "modrinth-app" {
        return Some(LauncherKind::Modrinth);
    }
    if name == "minecraft.exe" && is_store_launcher_process(proc.exe()) {
        return Some(LauncherKind::Official);
    }
    let exe = proc.exe();
    if crate::adapters::tlauncher::is_launcher_process(exe) {
        return Some(LauncherKind::Tlauncher);
    }
    if crate::adapters::legacy::is_install_process(exe) {
        return Some(LauncherKind::Legacy);
    }
    None
}

/// Процесс официального лаунчера — `Minecraft.exe` из папки, где рядом лежит
/// `gamelaunchhelper.exe`.
///
/// По одному имени файла нельзя: `Minecraft.exe` называется и у Bedrock.
/// По пути тоже: настоящую папку Store-пакета заранее не узнать — в реестре
/// лежит виртуальная `C:\Program Files\WindowsApps\…`, а процесс сообщает
/// реальный диск (проверено вживую: `D:\XboxGames\Minecraft Launcher\Content\
/// Minecraft.exe`, потому что приложения ставятся на D:). Поэтому опознаём по
/// содержимому папки: у лаунчера рядом с exe есть `gamelaunchhelper.exe`.
/// Проверка локальная и дешёвая, а не «доверимся имени пакета».
fn is_store_launcher_process(exe: Option<&Path>) -> bool {
    let Some(exe) = exe else {
        return false;
    };
    if !exe
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("minecraft.exe"))
    {
        return false;
    }
    exe.parent()
        .is_some_and(|dir| dir.join("gamelaunchhelper.exe").is_file())
}

/// Аккуратно закрывает лаунчер перед установкой: просит процессы завершиться
/// и ждёт их ухода (до ~10 с). Возвращает `true`, если лаунчер был запущен, —
/// фронт по этому флагу решает, открывать ли его обратно. Был закрыт —
/// `false`, трогать после установки не надо. Не-закрытие — не ошибка:
/// установка продолжается (fail-open, как раньше у Modrinth).
pub fn close_launcher(kind: LauncherKind) -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    if kind == LauncherKind::Manual {
        return false;
    }
    let mut sys = System::new();
    let mut was_running = false;
    for _ in 0..40 {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut alive = false;
        for proc in sys.processes().values() {
            if process_kind(proc) != Some(kind) {
                continue;
            }
            alive = true;
            was_running = true;
            proc.kill();
        }
        if !alive {
            return was_running;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    was_running
}

/// Открывает лаунчер после установки (без аргументов — просто окно приложения).
/// Только если он был запущен на старте (флаг от `close_launcher` — его
/// хранит фронт): закрытый не трогаем. Возвращает команду для «Подробнее».
pub fn reopen_launcher(exe: &Path) -> Result<String, crate::domain::InstallError> {
    if !exe.is_file() {
        return Err(crate::domain::InstallError::NoLauncher);
    }
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| crate::domain::InstallError::LaunchFailed(format!("{}: {e}", exe.display())))?;
    Ok(exe.display().to_string())
}

/// Exe Prism из регистрации протокола `prismlauncher://`.
#[cfg(windows)]
fn exe_from_protocol() -> Option<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_CLASSES_ROOT] {
        let hk = RegKey::predef(root);
        for class in ["prismlauncher", "PrismLauncher"] {
            let path = format!(r"{class}\shell\open\command");
            if let Ok(key) = hk.open_subkey(&path) {
                if let Ok(cmd) = key.get_value::<String, _>("") {
                    if let Some(exe) = first_quoted_or_bare(&cmd) {
                        if exe.is_file() {
                            return Some(exe);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn exe_from_protocol() -> Option<PathBuf> {
    None
}

/// Вытаскивает путь exe из командной строки вида `"C:\...\app.exe" "%1"`.
fn first_quoted_or_bare(cmd: &str) -> Option<PathBuf> {
    let cmd = cmd.trim();
    if let Some(rest) = cmd.strip_prefix('"') {
        return rest.split('"').next().map(PathBuf::from);
    }
    cmd.split_whitespace().next().map(PathBuf::from)
}

fn env_dir(var: &str, sub: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(|base| Path::new(&base).join(sub))
}

fn known_mmc_exe(fam: &MmcFamily) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        match fam.kind {
            LauncherKind::Prism => {
                for c in [
                    env_dir("LOCALAPPDATA", r"Programs\PrismLauncher\prismlauncher.exe"),
                    env_dir("ProgramFiles", r"PrismLauncher\prismlauncher.exe"),
                    env_dir("ProgramFiles(x86)", r"PrismLauncher\prismlauncher.exe"),
                    env_dir("USERPROFILE", r"scoop\apps\prismlauncher\current\prismlauncher.exe"),
                ] {
                    out.extend(c);
                }
            }
            LauncherKind::Pinecone => {
                for c in [
                    env_dir(
                        "LOCALAPPDATA",
                        r"Programs\ElyPrismLauncher\elyprismlauncher.exe",
                    ),
                    env_dir("ProgramFiles", r"ElyPrismLauncher\elyprismlauncher.exe"),
                    env_dir("LOCALAPPDATA", r"Programs\PineconeMC\pineconemc.exe"),
                ] {
                    out.extend(c);
                }
            }
            LauncherKind::Polymc => {
                for c in [
                    env_dir("LOCALAPPDATA", r"Programs\PolyMC\polymc.exe"),
                    env_dir("ProgramFiles", r"PolyMC\polymc.exe"),
                ] {
                    out.extend(c);
                }
            }
            // У MultiMC штатного установщика нет — только портативка, её ловят
            // процессы/ярлыки/PATH/ручные корни.
            LauncherKind::Multimc => {}
            LauncherKind::Modrinth | LauncherKind::Manual | LauncherKind::Tlauncher | LauncherKind::Legacy | LauncherKind::Official => {}
        }
    }
    #[cfg(not(windows))]
    {
        for prefix in fam.exe_prefixes {
            for c in [
                Some(PathBuf::from(format!("/usr/bin/{prefix}"))),
                Some(PathBuf::from(format!("/usr/local/bin/{prefix}"))),
                dirs::home_dir().map(|h| h.join(format!(".local/bin/{prefix}"))),
            ] {
                out.extend(c);
            }
        }
        if fam.kind == LauncherKind::Prism {
            out.extend(
                dirs::home_dir().map(|h| h.join("Applications/prismlauncher/prismlauncher")),
            );
        }
    }
    out.into_iter().filter(|p| p.is_file()).collect()
}

fn known_modrinth_exe() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        out.extend(env_dir("LOCALAPPDATA", r"Modrinth App\Modrinth App.exe"));
    }
    out.into_iter().filter(|p| p.is_file()).collect()
}

fn known_mmc_data_root(fam: &MmcFamily) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            for dir in fam.data_dirs {
                let c = appdata.join(dir);
                if is_mmc_data_dir(&c, fam) {
                    return Some(c);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        for dir in fam.data_dirs {
            if let Some(c) = dirs::data_dir()
                .map(|d| d.join(dir))
                .filter(|p| is_mmc_data_dir(p, fam))
            {
                return Some(c);
            }
        }
        // Flatpak: известен только у Prism и PolyMC.
        let flatpak: Option<&str> = match fam.kind {
            LauncherKind::Prism => Some("org.prismlauncher.PrismLauncher"),
            LauncherKind::Polymc => Some("org.polymc.PolyMC"),
            _ => None,
        };
        if let Some(id) = flatpak {
            if let Some(c) = dirs::home_dir()
                .map(|h| {
                    h.join(format!(".var/app/{id}/data/{}", fam.data_dirs[0]))
                })
                .filter(|p| is_mmc_data_dir(p, fam))
            {
                return Some(c);
            }
        }
        None
    }
}

fn known_modrinth_data_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        env_dir("APPDATA", "ModrinthApp").filter(|p| is_modrinth_data_dir(p))
    }
    #[cfg(not(windows))]
    {
        dirs::data_dir()
            .map(|d| d.join("ModrinthApp"))
            .filter(|p| is_modrinth_data_dir(p))
    }
}

/// Портативка: данные рядом с exe в приоритете над стандартным корнем.
/// Проверяем маркерами нужного лаунчера, имя exe уже проверено вызывателем.
fn data_root_next_to_exe(exe: &Path, kind: LauncherKind) -> Option<PathBuf> {
    let is_data = |dir: &Path| match kind {
        LauncherKind::Prism | LauncherKind::Pinecone | LauncherKind::Polymc | LauncherKind::Multimc => {
            is_mmc_data_dir(dir, mmc_family(kind))
        }
        LauncherKind::Modrinth => is_modrinth_data_dir(dir),
        // Ручная папка и TLauncher разбираются отдельно (см. ниже).
        LauncherKind::Manual | LauncherKind::Tlauncher | LauncherKind::Legacy | LauncherKind::Official => false,
    };
    let dir = exe.parent()?;
    if is_data(dir) {
        return Some(dir.to_path_buf());
    }
    // Сама папка exe может быть подпапкой портативки — один уровень вверх.
    if let Some(parent) = dir.parent() {
        if is_data(parent) {
            return Some(parent.to_path_buf());
        }
    }
    None
}

/// Exe из пользовательских ярлыков (без рекурсии вглубь диска).
#[cfg(windows)]
fn start_menu_exes() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = env_dir(
        "APPDATA",
        r"Microsoft\Windows\Start Menu\Programs",
    ) {
        dirs.push(p);
    }
    if let Some(p) = env_dir(
        "PROGRAMDATA",
        r"Microsoft\Windows\Start Menu\Programs",
    ) {
        dirs.push(p);
    }
    if let Some(p) = env_dir("USERPROFILE", r"Desktop") {
        dirs.push(p);
    }
    let mut out = Vec::new();
    for dir in dirs {
        collect_lnk_targets(&dir, 1, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(not(windows))]
fn start_menu_exes() -> Vec<PathBuf> {
    Vec::new()
}

/// Цели `*.lnk` в папке; `depth` — сколько уровней подпапок ещё можно.
#[cfg(windows)]
fn collect_lnk_targets(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    // Детерминированный порядок для стабильных тестов и UI.
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth > 0 {
                collect_lnk_targets(&path, depth - 1, out);
            }
            continue;
        }
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            if let Some(target) = lnk_target(&path) {
                if target.is_file() {
                    out.push(target);
                }
            }
        }
    }
}

/// Цель ярлыка `.lnk` (MS-SHLLINK): `LocalBasePath` из LinkInfo.
/// Возвращает `None` для битых ярлыков, папок и сетевых целей.
#[cfg(windows)]
fn lnk_target(path: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 78 || &bytes[0..4] != [0x4C, 0x00, 0x00, 0x00] {
        return None; // не ShellLink
    }
    let flags = u32::from_le_bytes(bytes[0x14..0x18].try_into().ok()?);
    let mut pos = 76usize;
    if flags & 0x01 != 0 {
        // HasLinkTargetIDList: пропускаем IDList (u16 размер + тело).
        if pos + 2 > bytes.len() {
            return None;
        }
        let id_size = u16::from_le_bytes(bytes[pos..pos + 2].try_into().ok()?) as usize;
        pos += 2 + id_size;
    }
    if flags & 0x02 == 0 {
        return None; // нет LinkInfo — цели-файла не достать
    }
    if pos + 0x1C > bytes.len() {
        return None;
    }
    let info_size = u32::from_le_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
    let info_flags = u32::from_le_bytes(bytes[pos + 8..pos + 12].try_into().ok()?);
    if info_flags & 0x01 == 0 {
        return None; // только сетевая цель — пропускаем
    }
    let base_off = u32::from_le_bytes(bytes[pos + 16..pos + 20].try_into().ok()?) as usize;
    if base_off == 0 || pos + base_off >= bytes.len() {
        return None;
    }
    // Предпочитаем Unicode-версию, если заголовок LinkInfo расширенный.
    let header_size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
    if header_size >= 0x24 && pos + 0x24 <= bytes.len() && pos + info_size <= bytes.len() {
        let uni_off =
            u32::from_le_bytes(bytes[pos + 0x1C..pos + 0x20].try_into().ok()?) as usize;
        if uni_off != 0 && pos + uni_off < bytes.len() {
            let end = bytes[pos + uni_off..pos + info_size]
                .chunks_exact(2)
                .position(|c| c == [0, 0])
                .map(|i| pos + uni_off + i * 2)
                .unwrap_or(pos + info_size);
            if let Ok(s) = String::from_utf16(
                &bytes[pos + uni_off..end]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<_>>(),
            ) {
                let p = PathBuf::from(s);
                if p.is_absolute() {
                    return Some(p);
                }
            }
        }
    }
    let start = pos + base_off;
    let end = bytes[start..].iter().position(|&b| b == 0).map(|i| start + i)?;
    let s = std::str::from_utf8(&bytes[start..end]).ok()?;
    let p = PathBuf::from(s);
    p.is_absolute().then_some(p)
}

/// Поиск exe по `PATH` (перехватывает нестандартные установки).
fn exes_from_path_search(fam: &MmcFamily) -> Vec<PathBuf> {
    #[cfg(windows)]
    let ext = ".exe";
    #[cfg(not(windows))]
    let ext = "";
    let mut out = Vec::new();
    let Some(path_var) = std::env::var_os("PATH") else {
        return out;
    };
    for dir in std::env::split_paths(&path_var) {
        for prefix in fam.exe_prefixes {
            let c = dir.join(format!("{prefix}{ext}"));
            if c.is_file() && is_mmc_exe(&c, fam) {
                out.push(c);
            }
        }
    }
    out
}

fn is_mmc_exe(p: &Path, fam: &MmcFamily) -> bool {
    p.is_file()
        && p.file_name().is_some_and(|n| {
            let lower = n.to_string_lossy().to_lowercase();
            fam.exe_prefixes.iter().any(|prefix| lower.starts_with(prefix))
        })
}

/// Exe портативной сборки лежит рядом с папкой данных (`MultiMC.exe` +
/// `instances/` в одной папке). Без этой проверки портативный MultiMC в
/// нестандартном месте выглядел бы как «приложение удалено».
fn mmc_exe_next_to_data(dir: &Path, fam: &MmcFamily) -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_mmc_exe(p, fam))
        .collect();
    found.sort();
    found.into_iter().next()
}

fn is_modrinth_exe(p: &Path) -> bool {
    p.is_file()
        && p.file_name().is_some_and(|n| {
            n.to_string_lossy().to_lowercase().starts_with("modrinth app")
        })
}

/// Корень данных Prism-семейства: есть `instances/` (инстансы появятся позже —
/// тоже ок, главное что это именно данные нужного лаунчера, а не случайная
/// папка) либо его cfg (`prismlauncher.cfg`, `polymc.cfg`, ...).
fn is_mmc_data_dir(p: &Path, fam: &MmcFamily) -> bool {
    p.is_dir()
        && (p.join("instances").is_dir()
            || fam.cfg_markers.iter().any(|m| p.join(m).is_file()))
}

fn is_any_mmc_data_dir(p: &Path) -> bool {
    [&PRISM, &PINECONE, &POLYMC, &MULTIMC]
        .into_iter()
        .any(|fam| is_mmc_data_dir(p, fam))
}

/// Владелец корня по cfg-маркеру (`polymc.cfg` → PolyMC...). Один корень —
/// одна семья: иначе ручной корень с общим `instances/` давал бы четыре
/// карточки-дубля. Bare-каталог (только `instances/`, без cfg) забирает
/// Prism — историческое поведение детекта.
fn mmc_owner(root: &Path) -> Option<LauncherKind> {
    [&PRISM, &PINECONE, &POLYMC, &MULTIMC]
        .into_iter()
        .find(|fam| fam.cfg_markers.iter().any(|m| root.join(m).is_file()))
        .map(|fam| fam.kind)
}

fn mmc_claim(root: &Path, kind: LauncherKind) -> bool {
    match mmc_owner(root) {
        Some(owner) => owner == kind,
        None => kind == LauncherKind::Prism,
    }
}

/// Корень данных Modrinth App: профили или база.
fn is_modrinth_data_dir(p: &Path) -> bool {
    p.is_dir() && (p.join("profiles").is_dir() || p.join("app.db").is_file())
}

/// Читает инстансы Prism-семейства: `instances/*/instance.cfg + mmc-pack.json`.
/// Инстансы без `net.minecraft` в компонентах пропускаем — по ним нельзя
/// построить строгий фильтр версия+загрузчик, гадать запрещено.
pub fn enumerate_mmc_targets(data_dir: &Path, kind: LauncherKind) -> Vec<GameTarget> {
    let mut out = Vec::new();
    let instances = data_dir.join("instances");
    let entries = std::fs::read_dir(&instances).map(|r| r.collect::<Vec<_>>());
    let mut dirs: Vec<PathBuf> = entries
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        if let Some(target) = read_mmc_instance(&dir, kind) {
            out.push(target);
        }
    }
    out
}

fn read_mmc_instance(dir: &Path, kind: LauncherKind) -> Option<GameTarget> {
    let cfg = std::fs::read_to_string(dir.join("instance.cfg")).ok()?;
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for line in cfg.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            values.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let mmc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("mmc-pack.json")).ok()?).ok()?;
    let components = mmc.get("components")?.as_array()?;

    let mut minecraft_version = None;
    let mut loader = LoaderKind::Vanilla;
    let mut loader_version = None;
    for c in components {
        let uid = c.get("uid")?.as_str()?;
        let version = c.get("version")?.as_str()?.to_string();
        match uid {
            "net.minecraft" => minecraft_version = Some(version),
            "net.minecraftforge" => {
                loader = LoaderKind::Forge;
                loader_version = Some(version);
            }
            "net.fabricmc.fabric-loader" => {
                loader = LoaderKind::Fabric;
                loader_version = Some(version);
            }
            "org.quiltmc.quilt-loader" => {
                loader = LoaderKind::Quilt;
                loader_version = Some(version);
            }
            "net.neoforged" | "net.neoforged.neoforge" => {
                loader = LoaderKind::NeoForge;
                loader_version = Some(version);
            }
            _ => {}
        }
    }
    Some(GameTarget {
        launcher: kind,
        name: values
            .remove("name")
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| dir_name(dir)),
        path: dir.to_path_buf(),
        minecraft_version: minecraft_version?,
        loader,
        loader_version,
    })
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "instance".to_string())
}

/// Маппинг строки `loader` из БД в доменный тип. Неизвестные — `None`
/// (цель пропускается: строгий фильтр важнее полноты списка).
fn modrinth_loader(s: &str) -> Option<LoaderKind> {
    match s.to_ascii_lowercase().as_str() {
        "vanilla" | "minecraft" => Some(LoaderKind::Vanilla),
        "fabric" => Some(LoaderKind::Fabric),
        "forge" => Some(LoaderKind::Forge),
        "quilt" => Some(LoaderKind::Quilt),
        "neoforge" => Some(LoaderKind::NeoForge),
        _ => None,
    }
}

/// Профили Modrinth App: `instances ⨝ instance_content_sets` по
/// `applied_content_set_id` (fallback — набор `Default`).
/// Открываем строго read-only: даже SELECT по WAL-базе без записи.
pub fn read_modrinth_targets(data_dir: &Path) -> Result<Vec<GameTarget>, crate::domain::InstallError> {
    use crate::domain::InstallError;
    let db_path = data_dir.join("app.db");
    if !db_path.is_file() {
        return Err(InstallError::NoDataDir {
            launcher: "Modrinth App (нет app.db)".to_string(),
        });
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    // Схема может отличаться между версиями приложения — проверяем таблицы.
    let has_tables = |names: &[&str]| -> Result<bool, InstallError> {
        for name in names {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![name],
                    |r| r.get(0),
                )
                .map_err(|e| InstallError::ModrinthSchema(e.to_string()))?;
            if n == 0 {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if !has_tables(&["instances", "instance_content_sets"])? {
        return Err(InstallError::ModrinthSchema(
            "нет таблиц instances/instance_content_sets".to_string(),
        ));
    }

    let profiles_dir = data_dir.join("profiles");
    let mut stmt = conn
        .prepare(
            "SELECT i.id, i.path, i.name, i.install_stage, i.applied_content_set_id,
                    c.game_version, c.loader, c.loader_version, c.name
             FROM instances i LEFT JOIN instance_content_sets c
               ON c.instance_id = i.id
              AND (c.id = i.applied_content_set_id OR (i.applied_content_set_id IS NULL AND c.name = 'Default'))",
        )
        .map_err(|e| InstallError::ModrinthSchema(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(|e| InstallError::ModrinthSchema(e.to_string()))?;

    let mut out = Vec::new();
    // Один инстанс может дать несколько строк (несколько content sets) —
    // берём первую валидную.
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    for row in rows {
        let (id, rel_path, name, stage, _applied, game_version, loader, loader_version) =
            row.map_err(|e| InstallError::ModrinthSchema(e.to_string()))?;
        if stage != "installed" || !seen.insert(id.clone()) {
            continue;
        }
        let (Some(game_version), Some(loader)) = (game_version, loader) else {
            continue;
        };
        let Some(loader) = modrinth_loader(&loader) else {
            continue; // неизвестный загрузчик — пропускаем, не гадаем
        };
        out.push(GameTarget {
            launcher: LauncherKind::Modrinth,
            name,
            path: profiles_dir.join(rel_path),
            minecraft_version: game_version,
            loader,
            loader_version,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// Папка вручную: валидна, если похожа на игру (saves/versions/mods/config).
fn manual_target(dir: &Path) -> Option<GameTarget> {
    if !dir.is_dir() {
        return None;
    }
    let looks_like_game = ["saves", "versions", "mods", "config", ".minecraft"]
        .iter()
        .any(|m| dir.join(m).is_dir());
    if !looks_like_game {
        return None;
    }
    let game_root = if dir.join(".minecraft").is_dir() {
        dir.join(".minecraft")
    } else {
        dir.to_path_buf()
    };
    Some(GameTarget {
        launcher: LauncherKind::Manual,
        name: dir_name(dir),
        path: game_root,
        minecraft_version: String::new(), // версия неизвестна — только ванилла
        loader: LoaderKind::Vanilla,
        loader_version: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-detect-{tag}"))
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdirs");
        std::fs::write(path, content).expect("write");
    }

    fn fabric_instance_cfg(name: &str) -> String {
        format!("ConfigVersion=1.3\nInstanceType=OneSix\nManagedPack=false\nname={name}\n")
    }

    /// Регрессия: лаунчер искался по подстроке имени пакета в пути, а реальный
    /// путь у Store-сборки другой диск (`D:\XboxGames\…`) — и сверху имя
    /// файла, у Bedrock совпадающее. Теперь опознаём по `gamelaunchhelper.exe`
    /// рядом с exe.
    #[test]
    fn store_launcher_process_needs_the_helper_beside_the_exe() {
        let root = temp_root("store-proc");
        let content = root.join("Minecraft Launcher").join("Content");
        std::fs::create_dir_all(&content).expect("mkdir");
        std::fs::write(content.join("Minecraft.exe"), b"x").expect("exe");
        std::fs::write(content.join("gamelaunchhelper.exe"), b"x").expect("helper");

        assert!(is_store_launcher_process(Some(&content.join("Minecraft.exe"))));

        // Bedrock: тот же `Minecraft.exe`, но помощника лаунчера рядом нет.
        let bedrock = root.join("MinecraftUWP").join("Content");
        std::fs::create_dir_all(&bedrock).expect("mkdir");
        std::fs::write(bedrock.join("Minecraft.exe"), b"x").expect("exe");
        assert!(!is_store_launcher_process(Some(&bedrock.join("Minecraft.exe"))));

        // Помощник есть, но имя exe чужое.
        assert!(!is_store_launcher_process(Some(
            &content.join("gamelaunchhelper.exe")
        )));
        assert!(!is_store_launcher_process(Some(
            &content.join("Minecraft.Windows.exe")
        )));
        assert!(!is_store_launcher_process(None));
    }

    fn mmc_pack(mc: &str, loader_uid: Option<(&str, &str)>) -> String {
        let mut components = format!(
            r#"{{"uid": "net.minecraft", "version": "{mc}", "important": true}}"#
        );
        if let Some((uid, ver)) = loader_uid {
            components.push_str(&format!(r#",{{"uid": "{uid}", "version": "{ver}"}}"#));
        }
        format!(r#"{{"formatVersion": 1, "components": [{components}]}}"#)
    }

    #[test]
    fn prism_targets_are_enumerated_with_versions() {
        let root = temp_root("prism");
        let inst = root.join("instances").join("Alpha");
        write(&inst.join("instance.cfg"), &fabric_instance_cfg("Alpha"));
        write(
            &inst.join("mmc-pack.json"),
            &mmc_pack("1.21.11", Some(("net.fabricmc.fabric-loader", "0.19.5"))),
        );
        std::fs::create_dir_all(inst.join("minecraft").join("saves")).expect("saves");
        // Битый инстанс без версии игры — пропускается молча.
        let broken = root.join("instances").join("Broken");
        write(&broken.join("instance.cfg"), &fabric_instance_cfg("Broken"));
        write(&broken.join("mmc-pack.json"), r#"{"formatVersion": 1, "components": []}"#);

        let targets = enumerate_mmc_targets(&root, LauncherKind::Prism);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "Alpha");
        assert_eq!(targets[0].minecraft_version, "1.21.11");
        assert_eq!(targets[0].loader, LoaderKind::Fabric);
        assert_eq!(targets[0].loader_version.as_deref(), Some("0.19.5"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn portable_data_next_to_exe_wins() {
        let root = temp_root("portable");
        let exe = root.join("prismlauncher.exe");
        std::fs::write(&exe, b"fake").expect("exe");
        std::fs::create_dir_all(root.join("instances")).expect("instances");
        let found =
            data_root_next_to_exe(&exe, LauncherKind::Prism).expect("portable root");
        assert_eq!(found, root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mmc_targets_carry_family_kind() {
        let root = temp_root("mmc-kind");
        let inst = root.join("instances").join("Alpha");
        write(&inst.join("instance.cfg"), &fabric_instance_cfg("Alpha"));
        write(
            &inst.join("mmc-pack.json"),
            &mmc_pack("1.21.11", Some(("net.fabricmc.fabric-loader", "0.19.5"))),
        );
        let targets = enumerate_mmc_targets(&root, LauncherKind::Pinecone);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].launcher, LauncherKind::Pinecone);
        assert_eq!(targets[0].minecraft_version, "1.21.11");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mmc_exe_prefixes_do_not_cross_match() {
        let root = temp_root("mmc-exe");
        for (name, fam, others) in [
            ("elyprismlauncher.exe", &PINECONE, [&PRISM, &POLYMC, &MULTIMC]),
            ("polymc.exe", &POLYMC, [&PRISM, &PINECONE, &MULTIMC]),
            ("MultiMC.exe", &MULTIMC, [&PRISM, &PINECONE, &POLYMC]),
            ("prismlauncher.exe", &PRISM, [&PINECONE, &POLYMC, &MULTIMC]),
        ] {
            let exe = root.join(name);
            std::fs::write(&exe, b"fake").expect("exe");
            assert!(is_mmc_exe(&exe, fam), "{name}");
            for other in others {
                assert!(!is_mmc_exe(&exe, other), "{name} vs {}", other.display);
            }
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mmc_data_dirs_match_by_instances_or_cfg() {
        let root = temp_root("mmc-data");
        // Портативный MultiMC: exe + instances/ + собственный cfg рядом.
        let portable = root.join("portable");
        std::fs::create_dir_all(&portable).expect("portable dir");
        let exe = portable.join("MultiMC.exe");
        std::fs::write(&exe, b"fake").expect("exe");
        std::fs::create_dir_all(portable.join("instances")).expect("instances");
        write(&portable.join("multimc.cfg"), "InstanceDir=instances\n");
        assert!(is_mmc_data_dir(&portable, &MULTIMC));
        assert!(is_mmc_data_dir(&portable, &POLYMC)); // маркер общий, владелец один
        assert!(mmc_claim(&portable, LauncherKind::Multimc));
        assert!(!mmc_claim(&portable, LauncherKind::Polymc));
        assert_eq!(
            data_root_next_to_exe(&exe, LauncherKind::Multimc).expect("root"),
            portable
        );
        // Установленный PolyMC: корень только с cfg, без instances/.
        let installed = root.join("PolyMC");
        write(&installed.join("polymc.cfg"), "InstanceDir=instances\n");
        assert!(is_mmc_data_dir(&installed, &POLYMC));
        assert!(mmc_claim(&installed, LauncherKind::Polymc));
        assert!(!mmc_claim(&installed, LauncherKind::Prism));
        // Bare-каталог (только instances/) — исторически Prism, без дублей.
        let bare = root.join("bare");
        std::fs::create_dir_all(bare.join("instances")).expect("bare instances");
        assert!(mmc_claim(&bare, LauncherKind::Prism));
        assert!(!mmc_claim(&bare, LauncherKind::Pinecone));
        // Чужой каталог корнем не считается.
        assert!(!is_any_mmc_data_dir(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn manual_mmc_root_yields_single_card() {
        let root = temp_root("mmc-single");
        let inst = root.join("instances").join("Alpha");
        write(&inst.join("instance.cfg"), &fabric_instance_cfg("Alpha"));
        write(
            &inst.join("mmc-pack.json"),
            &mmc_pack("1.21.11", Some(("net.fabricmc.fabric-loader", "0.19.5"))),
        );
        write(&root.join("polymc.cfg"), "InstanceDir=instances\n");
        let detection = detect_launchers(&[(*root).clone()], &[]);
        let ours: Vec<_> = detection
            .launchers
            .iter()
            .filter(|l| l.data_dir == *root)
            .collect();
        assert_eq!(ours.len(), 1);
        assert_eq!(ours[0].kind, LauncherKind::Polymc);
        assert_eq!(ours[0].targets.len(), 1);
        assert_eq!(ours[0].targets[0].launcher, LauncherKind::Polymc);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn manual_kind_never_closes() {
        assert!(!close_launcher(LauncherKind::Manual));
    }

    #[test]
    fn reopen_without_exe_is_an_error() {
        assert!(reopen_launcher(Path::new("")).is_err());
        assert!(reopen_launcher(&std::env::temp_dir().join("kraimaps-no-such.exe")).is_err());
    }

    #[test]
    fn same_data_dir_dedupes_to_one() {
        let root = temp_root("dedupe");
        let roots = vec![
            DataRoot {
                exe: None,
                dir: root.clone(),
                portable: false,
            },
            DataRoot {
                exe: None,
                dir: root.clone(),
                portable: false,
            },
        ];
        assert_eq!(dedup_roots(roots).len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn different_data_dirs_stay_separate() {
        let a = temp_root("multi-a");
        let b = temp_root("multi-b");
        let roots = vec![
            DataRoot {
                exe: None,
                dir: a.clone(),
                portable: false,
            },
            DataRoot {
                exe: None,
                dir: b.clone(),
                portable: true,
            },
        ];
        let deduped = dedup_roots(roots);
        assert_eq!(deduped.len(), 2);
        assert!(deduped.iter().any(|r| r.portable));
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }

    /// Минимальный синтетический .lnk: заголовок + LinkInfo с LocalBasePath.
    #[cfg(windows)]
    fn synthetic_lnk(target: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 76];
        bytes[0] = 0x4C; // magic ShellLink
        bytes[0x14] = 0x02; // HasLinkInfo
        let target_bytes = target.as_bytes();
        let info_size = 0x1Cu32 + target_bytes.len() as u32 + 1;
        let mut info = vec![0u8; info_size as usize];
        info[0..4].copy_from_slice(&info_size.to_le_bytes());
        info[4..8].copy_from_slice(&0x1Cu32.to_le_bytes());
        info[8..12].copy_from_slice(&0x01u32.to_le_bytes()); // VolumeID+Local
        info[16..20].copy_from_slice(&0x1Cu32.to_le_bytes()); // LocalBasePathOffset
        info[0x1C..0x1C + target_bytes.len()].copy_from_slice(target_bytes);
        bytes.extend_from_slice(&info);
        bytes
    }

    #[test]
    #[cfg(windows)]
    fn lnk_target_reads_local_base_path() {
        let root = temp_root("lnk");
        let lnk = root.join("Prism Launcher.lnk");
        std::fs::write(&lnk, synthetic_lnk(r"C:\Portable\Prism\prismlauncher.exe"))
            .expect("lnk");
        assert_eq!(
            lnk_target(&lnk),
            Some(PathBuf::from(r"C:\Portable\Prism\prismlauncher.exe"))
        );
        // Битый файл ярлыком не считается.
        let broken = root.join("broken.lnk");
        std::fs::write(&broken, b"not a shortcut").expect("broken");
        assert_eq!(lnk_target(&broken), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn command_line_exe_is_parsed() {
        let exe = first_quoted_or_bare(r#""C:\Apps\Prism\prismlauncher.exe" "%1""#).expect("exe");
        assert_eq!(exe, PathBuf::from(r"C:\Apps\Prism\prismlauncher.exe"));
    }

    #[test]
    fn modrinth_targets_come_from_sqlite() {
        let root = temp_root("modrinth");
        let db_path = root.join("app.db");
        {
            let conn = rusqlite::Connection::open(&db_path).expect("create db");
            conn.execute_batch(
                "CREATE TABLE instances (id TEXT PRIMARY KEY, path TEXT NOT NULL,
                   applied_content_set_id TEXT NULL, install_stage TEXT NOT NULL,
                   name TEXT NOT NULL, created INTEGER NOT NULL, last_played INTEGER NULL);
                 CREATE TABLE instance_content_sets (id TEXT PRIMARY KEY, instance_id TEXT NOT NULL,
                   name TEXT NOT NULL, source_kind TEXT NOT NULL, status TEXT NOT NULL,
                   game_version TEXT NOT NULL, loader TEXT NOT NULL, loader_version TEXT NULL,
                   created INTEGER NOT NULL, modified INTEGER NOT NULL);",
            )
            .expect("schema");
            conn.execute(
                "INSERT INTO instances VALUES ('local:uuid-1','Vanilla Jade','local:uuid-1:default','installed','Vanilla Jade',1,1)",
                [],
            )
            .expect("row");
            conn.execute(
                "INSERT INTO instance_content_sets VALUES ('local:uuid-1:default','local:uuid-1','Default','local','available','1.21.11','vanilla',NULL,1,1)",
                [],
            )
            .expect("row");
            // Недоустановленный и с неизвестным загрузчиком — оба пропускаются.
            conn.execute(
                "INSERT INTO instances VALUES ('local:uuid-2','Half','local:uuid-2:default','installing','Half',1,1)",
                [],
            )
            .expect("row");
            conn.execute(
                "INSERT INTO instance_content_sets VALUES ('local:uuid-2:default','local:uuid-2','Default','local','available','1.21.11','vanilla',NULL,1,1)",
                [],
            )
            .expect("row");
            conn.execute(
                "INSERT INTO instances VALUES ('local:uuid-3','Weird','local:uuid-3:default','installed','Weird',1,1)",
                [],
            )
            .expect("row");
            conn.execute(
                "INSERT INTO instance_content_sets VALUES ('local:uuid-3:default','local:uuid-3','Default','local','available','1.21.11','fancyloader',NULL,1,1)",
                [],
            )
            .expect("row");
        }
        let targets = read_modrinth_targets(&root).expect("targets");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "Vanilla Jade");
        assert_eq!(targets[0].minecraft_version, "1.21.11");
        assert_eq!(targets[0].loader, LoaderKind::Vanilla);
        assert!(targets[0].path.ends_with("Vanilla Jade"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn manual_folder_needs_game_markers() {
        let root = temp_root("manual");
        assert!(manual_target(&root).is_none());
        std::fs::create_dir_all(root.join("saves")).expect("saves");
        let target = manual_target(&root).expect("target");
        assert_eq!(target.loader, LoaderKind::Vanilla);
        std::fs::remove_dir_all(&root).ok();
    }

    fn tlauncher_root_with_fabric(tag: &str) -> crate::test_utils::TempDir {
        let root = temp_root(tag);
        let vdir = root.join("versions").join("fabric-loader-0.19.5-1.20.1");
        std::fs::create_dir_all(&vdir).expect("versions");
        write(
            &vdir.join("fabric-loader-0.19.5-1.20.1.json"),
            r#"{"inheritsFrom": "1.20.1",
                "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries": [{"name": "net.fabricmc:fabric-loader:0.19.5"}],
                "id": "fabric-loader-0.19.5-1.20.1", "type": "release"}"#,
        );
        std::fs::write(root.join("TLauncher.exe"), b"fake").expect("exe");
        root
    }

    #[test]
    fn tlauncher_root_is_detected_with_targets() {
        let root = tlauncher_root_with_fabric("tl");
        let found = detect_tlauncher(&[(*root).clone()], &[]);
        let ours = found.iter().find(|l| l.data_dir == root).expect("tl root");
        assert_eq!(ours.kind, LauncherKind::Tlauncher);
        assert_eq!(ours.exe_path, Some(root.join("TLauncher.exe")));
        assert!(!ours.portable);
        assert_eq!(ours.targets.len(), 1);
        assert_eq!(ours.targets[0].minecraft_version, "1.20.1");
        assert_eq!(ours.targets[0].loader, LoaderKind::Fabric);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn official_launcher_dir_is_not_tlauncher() {
        // versions/ + launcher_profiles.json без сигналов TL — чужой клиент.
        let root = temp_root("official");
        std::fs::create_dir_all(root.join("versions")).expect("versions");
        write(&root.join("launcher_profiles.json"), "{}");
        let found = detect_tlauncher(&[], &[]);
        assert!(found.iter().all(|l| l.data_dir != root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tlauncher_data_dir_beats_manual_target() {
        // Корень TL не должен дублироваться карточкой «Папка вручную».
        let root = tlauncher_root_with_fabric("tl-nodupe");
        let detection = detect_launchers(&[(*root).clone()], &[]);
        assert!(
            detection
                .launchers
                .iter()
                .filter(|l| l.kind == LauncherKind::Tlauncher)
                .any(|l| l.data_dir == root)
        );
        assert!(
            detection
                .launchers
                .iter()
                .all(|l| !(l.kind == LauncherKind::Manual && l.data_dir == root))
        );
        std::fs::remove_dir_all(&root).ok();
    }
    /// Живой прогон детекта по реальной машине. Не в обычном прогоне: читает
    /// настоящие `%APPDATA%` и системные процессы. Запуск: `cargo test -- --ignored`.
    #[test]
    #[ignore = "живой прогон по системе; запускается вручную"]
    fn live_detection_smoke() {
        let d = detect_launchers(&[], &[]);
        println!("запущены: {:?}", d.running);
        for l in &d.launchers {
            println!(
                "{:?} | {} | exe={:?} | app_found={} | целей={} | data_dir={}",
                l.kind,
                l.display_name,
                l.exe_path,
                l.app_found,
                l.targets.len(),
                l.data_dir.display()
            );
            for t in l.targets.iter().take(6) {
                println!(
                    "    - {} (mc {}, {:?}{}) -> {}",
                    t.name,
                    t.minecraft_version,
                    t.loader,
                    t.loader_version
                        .as_ref()
                        .map(|v| format!(" {v}"))
                        .unwrap_or_default(),
                    t.path.display()
                );
            }
        }
        assert!(d.launchers.iter().any(|l| l.kind == LauncherKind::Official));
    }
}