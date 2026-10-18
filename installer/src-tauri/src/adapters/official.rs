//! Официальный Minecraft Launcher.
//!
//! Отличие от TLauncher и Legacy Launcher принципиальное: у него есть
//! «Установки» (Installations) — профили в `launcher_profiles.json`, и у
//! каждой **своя папка игры** (`gameDir`). Это изоляция уровня Prism, а не
//! общая папка, поэтому здесь модель инстанса, а не «версии без инстансов»:
//! мы создаём СВОЮ установку с отдельным `gameDir` и больше ничего в
//! `launcher_profiles.json` не трогаем — ни имя, ни иконку, ни `lastUsed`
//! чужих установок.
//!
//! Запуск: у Store-сборки нет документированного CLI запуска профиля, зато
//! есть Quick Play — панель иконок у кнопки Play, наполняемая файлом
//! `launcher_quick_play.json`. Прописываем туда нашу установку и мир: клик по
//! иконке открывает игру сразу в нужном мире.

use super::vanilla;
use crate::domain::{GameTarget, InstallError, LauncherKind, LoaderKind};
use crate::services::java;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Имя файла с установками.
const PROFILES_FILE: &str = "launcher_profiles.json";
/// Панель Quick Play (формат задокументирован на minecraft.wiki).
const QUICK_PLAY_FILE: &str = "launcher_quick_play.json";
/// Каталог наших установок внутри корня `.minecraft`.
const INSTALL_DIR_NAME: &str = "KraiMaps";
/// Требуемый размер кастомной иконки (лаунчер показывает только 128×128).
const ICON_SIZE: u32 = 128;
/// AUMID Store-сборки лаунчера (`Get-StartApps` → `!Minecraft`).
pub const STORE_AUMID: &str = "Microsoft.4297127D64EC6_8wekyb3d8bbwe!Minecraft";

/// Имя каталога Store-пакета (PFN) в `%LOCALAPPDATA%\Packages`. Выводится из
/// AUMID, а не хранится рядом отдельной строкой: AUMID — единственный источник
/// истины, а две константы PFN разъедутся при первой же смене пакета.
fn package_family() -> &'static str {
    STORE_AUMID.split('!').next().unwrap_or(STORE_AUMID)
}

/// Корневой файл настроек лаунчера. Есть только у настоящего официального
/// лаунчера — в отличие от `launcher_profiles.json`, который пишет ещё и
/// установщик Forge в чужую папку.
const SETTINGS_FILE: &str = "launcher_settings.json";

/// Корень данных официального лаунчера: `%APPDATA%\.minecraft` с
/// `versions/`, файлом установок И файлом настроек лаунчера.
///
/// Проверки на «не моё» обязательны: папка игры TLauncher и Legacy Launcher
/// устроена так же и там тоже бывает `launcher_profiles.json`. Без них одна
/// папка отдавала две карточки — официальный лаунчер претендовал на чужое
/// игровое дерево.
pub fn is_data_dir(p: &Path) -> bool {
    if !p.is_dir()
        || !p.join("versions").is_dir()
        || !p.join(PROFILES_FILE).is_file()
        || !p.join(SETTINGS_FILE).is_file()
    {
        return false;
    }
    // `tlauncher_profiles.json` — профили аккаунтов TLauncher/Legacy.
    if p.join("tlauncher_profiles.json").is_file() {
        return false;
    }
    // `TLauncher*.exe` в корне игровых данных — признак TLauncher.
    !p.join("TLauncher.exe").is_file()
}

/// Стандартный корень `%APPDATA%\.minecraft`.
pub fn default_data_dir(roaming: &Path) -> PathBuf {
    roaming.join(".minecraft")
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Копия файла перед записью. В `launcher_profiles.json` лежат данные аккаунтов
/// (`mojangClientToken`), а в `launcher_quick_play.json` — история миров:
/// потерять их нельзя, поэтому пишем только после бэкапа.
fn backup(path: &Path) -> Result<PathBuf, InstallError> {
    if !path.is_file() {
        return Ok(path.to_path_buf());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dest = std::env::temp_dir().join(format!(
        "mapinstaller-official-{}-{stamp}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state")
    ));
    std::fs::copy(path, &dest)?;
    Ok(dest)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), InstallError> {
    backup(path)?;
    std::fs::write(path, serde_json::to_string_pretty(value).expect("json"))?;
    Ok(())
}

/// Установка лаунчера, прочитанная из `launcher_profiles.json`.
#[derive(Debug, Clone)]
pub struct Installation {
    pub id: String,
    pub name: String,
    pub version_id: String,
    /// Папка игры установки; `None` — дефолт, то есть сам корень.
    pub game_dir: Option<PathBuf>,
}

impl Installation {
    pub fn dir_in(&self, data_dir: &Path) -> PathBuf {
        self.game_dir.clone().unwrap_or_else(|| data_dir.to_path_buf())
    }
}

/// Все установки, кроме псевдо-`latest-release`/`latest-snapshot`:
/// у них нет своей папки, они всегда пишут в корень.
pub fn read_installations(data_dir: &Path) -> Vec<Installation> {
    let Some(doc) = read_json(&data_dir.join(PROFILES_FILE)) else {
        return Vec::new();
    };
    let Some(profiles) = doc.get("profiles").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, p) in profiles {
        let version_id = p
            .get("lastVersionId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if version_id.is_empty() {
            continue;
        }
        out.push(Installation {
            id: id.clone(),
            name: p
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(id.as_str())
                .to_string(),
            version_id,
            game_dir: p
                .get("gameDir")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        });
    }
    out.sort_by_key(|x| x.name.to_lowercase());
    out
}

/// Последняя стабильная версия из локальной копии манифеста Mojang —
/// чтобы «latest-release» показать как конкретную версию.
fn latest_release_version(data_dir: &Path) -> Option<String> {
    let doc = read_json(&data_dir.join("versions").join("version_manifest_v2.json"))?;
    doc.get("latest")?
        .get("release")?
        .get("id")?
        .as_str()
        .map(|s| s.to_string())
}

/// Цели лаунчера — его установки плюс корневая папка игры под «latest
/// release». Установки без своей `gameDir` указывают на корень, и это
/// нормально: так пользователь ставит обычные ванильные карты в свою папку.
pub fn enumerate_targets(data_dir: &Path) -> Vec<GameTarget> {
    let mut out = Vec::new();
    for inst in read_installations(data_dir) {
        let vdir = data_dir.join("versions").join(&inst.version_id);
        let Some(v) = vanilla::read_version_dir(&vdir) else {
            continue;
        };
        out.push(GameTarget {
            launcher: LauncherKind::Official,
            name: inst.name.clone(),
            path: inst.dir_in(data_dir),
            minecraft_version: v.mc_version,
            loader: v.loader,
            loader_version: v.loader_version,
        });
    }
    // Псевдо-установки лаунчер не показывает как отдельные карточки, но карта
    // ванильной версии должна попадать куда-то: в корень игры. Добавляем один
    // синтетический target, чтобы ванильная карта нашлась всегда.
    if let Some(mc) = latest_release_version(data_dir) {
        if !out.iter().any(|t| t.minecraft_version == mc && t.loader == LoaderKind::Vanilla) {
            out.push(GameTarget {
                launcher: LauncherKind::Official,
                name: "Minecraft (последняя версия)".to_string(),
                path: data_dir.to_path_buf(),
                minecraft_version: mc,
                loader: LoaderKind::Vanilla,
                loader_version: None,
            });
        }
    }
    out.sort_by_key(|x| x.name.to_lowercase());
    out
}

/// Путь нашей установки под карту: `<корень>/KraiMaps/<slug>`.
pub fn installation_dir(data_dir: &Path, slug: &str) -> PathBuf {
    data_dir.join(INSTALL_DIR_NAME).join(crate::domain::slugify(slug))
}

/// Кастомная иконка установки: PNG строго 128×128 в base64 с data-URI
/// префиксом (лаунчер принимает только такой формат; голый base64 и строки
/// вида `"Grass"` — прочие ветки, см. вики minecraft.wiki про Quick Play).
/// Возвращает `None`, если картинки нет или она нечитаема — установка
/// создаётся без иконки, это лучше, чем битая картинка.
pub fn icon_data_uri(png_path: Option<&Path>) -> Option<String> {
    use base64::Engine;
    let bytes = std::fs::read(png_path?).ok()?;
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?;
    let out = encode_png_exact(&img);
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(out)
    ))
}

/// PNG ровно `ICON_SIZE`×`ICON_SIZE`: картинку вписываем по меньшей стороне и
/// кладём по центру прозрачного холста. Лаунчер отказывается показывать
/// иконку другого размера, поэтому «почти 128×128» не подходит.
fn encode_png_exact(img: &image::DynamicImage) -> Vec<u8> {
    let size = ICON_SIZE as i64;
    let (w, h) = (img.width() as i64, img.height() as i64);
    let scale = (size as f64 / w.max(1) as f64).min(size as f64 / h.max(1) as f64);
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    let scaled = img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3);

    let mut canvas = image::RgbaImage::from_pixel(ICON_SIZE, ICON_SIZE, image::Rgba([0, 0, 0, 0]));
    let offset = (
        i64::from((ICON_SIZE - nw) / 2),
        i64::from((ICON_SIZE - nh) / 2),
    );
    image::imageops::overlay(&mut canvas, &scaled.to_rgba8(), offset.0, offset.1);

    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(canvas)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .expect("кодирование PNG в память не падает");
    cursor.into_inner()
}

/// Создание нашей установки: собственная папка игры под карту, свой id,
/// своя иконка. Существующие установки пользователя не трогаем.
///
/// Поле `gameDir` — обязательное: без него лаунчер запускает установку в
/// корне `.minecraft` и наша папка остаётся невидимой. Повторный вызов с тем
/// же именем обновляет уже созданную нами установку, а не плодит дубли.
pub fn create_installation(
    data_dir: &Path,
    display_name: &str,
    version_id: &str,
    icon: Option<&str>,
) -> Result<Installation, InstallError> {
    let profiles_path = data_dir.join(PROFILES_FILE);
    let mut doc = read_json(&profiles_path).ok_or(InstallError::NoDataDir {
        launcher: "Minecraft Launcher (нет launcher_profiles.json)".to_string(),
    })?;
    let game_dir = installation_dir(data_dir, display_name);
    std::fs::create_dir_all(&game_dir)?;
    let game_dir_str = game_dir.to_string_lossy().into_owned();

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut profile = json!({
        "type": "custom",
        "name": display_name,
        "lastVersionId": version_id,
        "created": now,
        "lastUsed": now,
        "gameDir": game_dir_str,
    });
    match icon {
        Some(data_uri) => {
            profile["icon"] = json!(data_uri);
        }
        None => {
            profile["icon"] = json!("Grass");
        }
    }
    let obj = doc
        .get_mut("profiles")
        .and_then(|p| p.as_object_mut())
        .ok_or_else(|| InstallError::ModrinthSchema("в profiles нет объекта".to_string()))?;

    // Установка с тем же именем и той же папкой игры — это наша прошлая
    // попытка для той же карты. Обновляем её, иначе каждая переустановка
    // оставляла бы после себя ещё одну копию в списке лаунчера.
    let existing = obj.iter().find_map(|(id, p)| {
        (p.get("name").and_then(|v| v.as_str()) == Some(display_name)
            && p.get("gameDir").and_then(|v| v.as_str()) == Some(game_dir_str.as_str()))
        .then(|| id.clone())
    });
    let seed = match existing {
        Some(id) => {
            let old_created = obj[&id]["created"].clone();
            let mut merged = profile;
            // `created` при обновлении не меняем — это дата первой установки.
            if !old_created.is_null() {
                merged["created"] = old_created;
            }
            obj.insert(id.clone(), merged);
            id
        }
        None => {
            // 32 hex — формат id лаунчера; генерируем свободный.
            let mut seed = uuid::Uuid::new_v4().simple().to_string();
            while obj.contains_key(&seed) {
                seed = uuid::Uuid::new_v4().simple().to_string();
            }
            obj.insert(seed.clone(), profile);
            seed
        }
    };
    write_json(&profiles_path, &doc)?;

    Ok(Installation {
        id: seed,
        name: display_name.to_string(),
        version_id: version_id.to_string(),
        game_dir: Some(game_dir),
    })
}

/// Включён ли Quick Play в настройках лаунчера. Без него панель пустая и
/// наш «Играть» не сработает — предупреждаем пользователя заранее.
pub fn quick_play_enabled(data_dir: &Path) -> bool {
    read_json(&data_dir.join(SETTINGS_FILE))
        .and_then(|d| d.get("quickPlayEnabled").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

/// Прописываем мир в панель Quick Play: пользователь кликает иконку — игра
/// открывается сразу в мире. Возвращает, включён ли Quick Play в настройках
/// лаунчера: если выключен, запись всё равно записывается (заработает после
/// включения), но фронт должен предупредить пользователя.
pub fn add_quick_play(
    data_dir: &Path,
    profile_id: &str,
    world_folder: &str,
    world_name: &str,
    icon: Option<&str>,
) -> Result<bool, InstallError> {
    let path = data_dir.join(QUICK_PLAY_FILE);
    let mut doc = read_json(&path).ok_or_else(|| InstallError::NoDataDir {
        launcher: "Minecraft Launcher (нет launcher_quick_play.json)".to_string(),
    })?;
    let quick = doc
        .get_mut("quickPlayData")
        .and_then(|q| q.as_object_mut())
        .ok_or_else(|| InstallError::ModrinthSchema("нет quickPlayData".to_string()))?;

    // Ключ верхнего уровня — remote id аккаунта Microsoft, и он не равняется
    // ни одному id из `launcher_profiles.json`: выдумывать нельзя. Берём тот
    // аккаунт, у которого уже есть запись нашей установки (значит, панель
    // настроена на него), иначе — первый.
    let account = quick
        .keys()
        .find(|k| {
            quick
                .get(*k)
                .and_then(|l| l.as_array())
                .is_some_and(|l| {
                    l.iter().any(|e| {
                        e.get("javaInstance").and_then(|j| j.get("configId")) == Some(&json!(profile_id))
                    })
                })
        })
        .or_else(|| quick.keys().next())
        .cloned()
        .ok_or_else(|| InstallError::NoDataDir {
            launcher: "Minecraft Launcher (Quick Play пуст — запустите лаунчер сначала)"
                .to_string(),
        })?;

    let mut entry = json!({
        "epochLastPlayedTimeMs": chrono::Utc::now().timestamp_millis(),
        "id": world_folder,
        "name": world_name,
        "source": "Java",
        "javaInstance": {
            "configId": profile_id,
            "game": { "type": "singleplayer", "gamemode": "survival" }
        },
    });
    if let Some(data_uri) = icon {
        entry["customizedIcon"] = json!(data_uri);
    }
    let list = quick
        .get_mut(&account)
        .and_then(|l| l.as_array_mut())
        .ok_or_else(|| InstallError::ModrinthSchema("quickPlayData не список".to_string()))?;
    list.retain(|e| !(e.get("javaInstance").and_then(|j| j.get("configId")) == Some(&json!(profile_id))
        && e.get("id") == Some(&json!(world_folder))));
    list.insert(0, entry);
    doc["version"] = json!(2);
    write_json(&path, &doc)?;
    Ok(quick_play_enabled(data_dir))
}

/// Подготовка версии и нашей установки под карту.
pub async fn ensure_installation(
    data_dir: &Path,
    display_name: &str,
    mc_version: &str,
    loader: LoaderKind,
    loader_version: Option<&str>,
    icon: Option<&str>,
    progress: crate::services::deploy::Progress<'_>,
) -> Result<vanilla::EnsureResult, InstallError> {
    if mc_version.trim().is_empty() {
        return Err(InstallError::UnknownLoader(
            "неизвестна версия игры для подготовки".to_string(),
        ));
    }
    let version_dir_name = if loader == LoaderKind::Vanilla {
        let existing = vanilla::read_version_dir(&data_dir.join("versions").join(mc_version))
            .map(|v| v.dir_name)
            .unwrap_or_else(|| mc_version.to_string());
        let _closed = crate::services::detect::close_launcher(LauncherKind::Official);
        existing
    } else {
        let loader_version = vanilla::require_exact_loader_version(loader, loader_version)?;
        let found = vanilla::find_match(data_dir, mc_version, loader, &loader_version);
        let _closed = crate::services::detect::close_launcher(LauncherKind::Official);
        match found {
            Some(v) => v.dir_name,
            None => {
                let roots = java_roots(data_dir);
                vanilla::provision(data_dir, mc_version, loader, &loader_version, &roots, progress)
                    .await?
            }
        }
    };

    let inst = create_installation(data_dir, display_name, &version_dir_name, icon)?;
    let game_dir = inst.dir_in(data_dir);
    Ok(vanilla::EnsureResult {
        target: GameTarget {
            launcher: LauncherKind::Official,
            name: inst.name.clone(),
            path: game_dir.clone(),
            minecraft_version: mc_version.to_string(),
            loader,
            loader_version: loader_version.map(|s| s.to_string()),
        },
        mods_empty: vanilla::mods_empty(&game_dir),
        // У официального лаунчера нет конфига с выбором версии: свежий
        // `lastUsed` в только что созданной установке заставляет лаунчер
        // выбрать её самому.
        preselected: true,
    })
}

/// Запуск лаунчера. `aumid` задан для Store-сборки (клик по панели Quick Play
/// всё равно оставляет лаунчер открытым), `exe` — для старой Standalone.
pub fn launch_official(exe: Option<&Path>) -> Result<String, InstallError> {
    if let Some(exe) = exe {
        if exe.is_file() {
            std::process::Command::new(exe).spawn().map_err(|e| {
                InstallError::LaunchFailed(format!("{}: {e}", exe.display()))
            })?;
            return Ok(exe.display().to_string());
        }
    }
    if !store_app_installed() {
        return Err(InstallError::LaunchFailed(
            "Minecraft Launcher не установлен из Microsoft Store".to_string(),
        ));
    }
    launch_by_aumid()
}

/// Установлен ли Store-пакет лаунчера. Без него запустить приложение по AUMID
/// нечем, и пользователю честнее сказать об этом, чем получить пустую ошибку
/// от `explorer.exe`.
pub fn store_app_installed() -> bool {
    dirs::data_local_dir()
        .map(|p| p.join("Packages").join(package_family()))
        .is_some_and(|p| p.is_dir())
}

/// Рантаймы официального лаунчера. Standalone- сборка держит их в
/// `<game_dir>/runtime`, а Store-сборка — в
/// `%LOCALAPPDATA%\Packages\<PFN>\LocalCache\Local\runtime`. Пути не
/// пересекаются, и какой именно стоит у пользователя, мы не выбирали,
/// поэтому проверяем обе раскладки.
pub fn java_roots(data_dir: &Path) -> java::JavaRoots {
    let mut mojang = vec![data_dir.join("runtime")];
    mojang.extend(store_runtime_roots());
    java::JavaRoots {
        mojang,
        bundled: Vec::new(),
    }
}

/// Папки с Store-рантаймами: сначала точный PFN из AUMID, затем ограниченный
/// проход по установленным пакетам — одна проверка `is_dir` на пакет, без
/// рекурсии. Так переживаем и переименование пакета, и другие MSIX-сборки
/// лаунчера. Если `%LOCALAPPDATA%` недоступен — пусто, это не ошибка.
fn store_runtime_roots() -> Vec<PathBuf> {
    let Some(local) = dirs::data_local_dir() else {
        return Vec::new();
    };
    let packages = local.join("Packages");
    let mut out = Vec::new();

    let exact = packages
        .join(package_family())
        .join("LocalCache")
        .join("Local")
        .join("runtime");
    if exact.is_dir() {
        out.push(exact);
    }

    let Ok(entries) = std::fs::read_dir(&packages) else {
        return out;
    };
    for entry in entries.flatten() {
        let runtime = entry.path().join("LocalCache").join("Local").join("runtime");
        if runtime.is_dir() && !out.contains(&runtime) {
            out.push(runtime);
        }
    }
    out
}

/// Запуск через AUMID: единственный надёжный способ поднять Store-сборку
/// (`WindowsApps` закрыт для прямого запуска из обычного приложения).
pub fn launch_by_aumid() -> Result<String, InstallError> {
    std::process::Command::new("explorer.exe")
        .arg(format!("shell:AppsFolder\\{STORE_AUMID}"))
        .spawn()
        .map_err(|e| InstallError::LaunchFailed(format!("shell:AppsFolder: {e}")))?;
    Ok(format!("shell:AppsFolder\\{STORE_AUMID}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-official-{tag}"))
    }

    fn make_data_dir(root: &Path) -> PathBuf {
        let mc = root.join(".minecraft");
        std::fs::create_dir_all(mc.join("versions")).expect("versions");
        std::fs::write(
            mc.join(PROFILES_FILE),
            r#"{"profiles": {"abc": {"type": "custom", "name": "Моя", "lastVersionId": "1.21.11",
                "icon": "Furnace", "gameDir": "C:\\mc\\KraiMaps\\DeepMine"}}, "version": 6}"#,
        )
        .expect("profiles");
        let vdir = mc.join("versions").join("1.21.11");
        std::fs::create_dir_all(&vdir).expect("vdir");
        std::fs::write(
            vdir.join("1.21.11.json"),
            r#"{"id": "1.21.11", "mainClass": "net.minecraft.client.main.Main",
                "libraries": [{"name": "com.mojang:patchy:2.2.10"}], "type": "release"}"#,
        )
        .expect("json");
        std::fs::write(
            mc.join("versions").join("version_manifest_v2.json"),
            r#"{"latest": {"release": {"id": "1.21.11", "type": "release"}, "snapshot": {"id": "26.4-snapshot-4", "type": "snapshot"}}}"#,
        )
        .expect("manifest");
        // Настройки лаунчера — обязательный маркер настоящего официального
        // лаунчера (в отличие от `launcher_profiles.json`, который пишет ещё
        // и установщик Forge).
        std::fs::write(
            mc.join(SETTINGS_FILE),
            r#"{"version": 17, "quickPlayEnabled": true}"#,
        )
        .expect("settings");
        mc
    }

    #[test]
    fn data_dir_requires_versions_and_profiles() {
        let root = temp_root("markers");
        let mc = make_data_dir(&root);
        assert!(is_data_dir(&mc));
        let bare = root.join("bare");
        std::fs::create_dir_all(bare.join("versions")).expect("v");
        assert!(!is_data_dir(&bare));
    }

    #[test]
    fn other_launchers_game_dir_is_not_official() {
        // Живой баг: папка игры TLauncher/Legacy Launcher тоже содержит
        // `versions/` и `launcher_profiles.json` (его пишет установщик Forge),
        // из-за чего официальный лаунчер показывал лишнюю карточку на чужих
        // данных — и даже подписывал их своим именем.
        let root = temp_root("foreign");
        let foreign = root.join("foreign-game");
        std::fs::create_dir_all(foreign.join("versions").join("1.21.11")).expect("v");
        std::fs::write(
            foreign.join("launcher_profiles.json"),
            r#"{"clientToken": "x", "profiles": {}}"#,
        )
        .expect("lp");
        assert!(
            !is_data_dir(&foreign),
            "чужая папка с launcher_profiles.json не должна считаться официальным лаунчером"
        );

        // Те же папки, но с маркерами TLauncher — тоже не наш.
        std::fs::write(foreign.join("tlauncher_profiles.json"), "{}").expect("tp");
        assert!(!is_data_dir(&foreign));
    }

    #[test]
    fn installations_are_read_with_game_dirs() {
        let root = temp_root("install");
        let mc = make_data_dir(&root);
        let insts = read_installations(&mc);
        assert_eq!(insts.len(), 1);
        assert_eq!(insts[0].id, "abc");
        assert_eq!(insts[0].name, "Моя");
        assert_eq!(
            insts[0].dir_in(&mc),
            PathBuf::from(r"C:\mc\KraiMaps\DeepMine")
        );
    }

    #[test]
    fn create_installation_adds_our_profile_without_touching_others() {
        let root = temp_root("create");
        let mc = make_data_dir(&root);
        let before = read_installations(&mc);
        let icon = icon_data_uri(None);
        let inst =
            create_installation(&mc, "Deep Mine 1.21.11", "1.21.11", icon.as_deref()).expect("create");

        let after = read_installations(&mc);
        assert_eq!(after.len(), before.len() + 1);
        // Чужая установка не тронута: тот же id, то же имя, тот же gameDir.
        let untouched = after.iter().find(|i| i.id == "abc").expect("kept");
        assert_eq!(untouched.name, "Моя");
        assert_eq!(untouched.dir_in(&mc), PathBuf::from(r"C:\mc\KraiMaps\DeepMine"));
        assert_eq!(inst.dir_in(&mc), installation_dir(&mc, "Deep Mine 1.21.11"));
        // Наша установка внутри корня .minecraft, не в общей папке.
        assert!(inst.dir_in(&mc).starts_with(mc.join(INSTALL_DIR_NAME)));
    }

    #[test]
    fn created_profile_is_well_formed() {
        let root = temp_root("shape");
        let mc = make_data_dir(&root);
        create_installation(&mc, "Map", "1.21.11", None).expect("create");
        let doc = read_json(&mc.join(PROFILES_FILE)).expect("doc");
        let ours = doc["profiles"]
            .as_object()
            .expect("profiles")
            .values()
            .find(|p| p["name"] == json!("Map"))
            .expect("ours");
        assert_eq!(ours["type"], "custom");
        assert_eq!(ours["lastVersionId"], "1.21.11");
        assert!(ours["icon"].is_string());
        assert!(ours["created"].is_string() && ours["lastUsed"].is_string());
        // Без `gameDir` лаунчер запустит установку в корне `.minecraft`, и наша
        // папка останется невидимой — это и был баг.
        assert_eq!(
            ours["gameDir"].as_str(),
            Some(installation_dir(&mc, "Map").to_string_lossy().as_ref()),
        );
    }

    #[test]
    fn repeat_install_reuses_profile_instead_of_duplicating() {
        let root = temp_root("repeat");
        let mc = make_data_dir(&root);
        let first = create_installation(&mc, "Map", "1.21.11", None).expect("create");
        let second = create_installation(&mc, "Map", "1.21.11", None).expect("recreate");
        assert_eq!(first.id, second.id, "повторная установка не плодит копии");
        assert_eq!(read_installations(&mc).len(), 2, "одна наша + одна исходная");

        // `created` первой установки сохраняется, `lastVersionId` обновляется.
        let doc = read_json(&mc.join(PROFILES_FILE)).expect("doc");
        let ours = &doc["profiles"][&first.id];
        assert_eq!(ours["name"], json!("Map"));
    }

    #[test]
    fn quick_play_adds_entry_for_our_installation() {
        let root = temp_root("quickplay");
        let mc = make_data_dir(&root);
        std::fs::write(
            mc.join(QUICK_PLAY_FILE),
            r#"{"quickPlayData": {"2535454739329515": [
                {"epochLastPlayedTimeMs": 1, "id": "Old", "javaInstance": {"configId": "zzz"}, "source": "Java"}]},
                "version": 2}"#,
        )
        .expect("qp");

        add_quick_play(&mc, "abc", "Deep Mine", "Deep Mine", None).expect("add");
        let doc = read_json(&mc.join(QUICK_PLAY_FILE)).expect("doc");
        let list = doc["quickPlayData"]["2535454739329515"].as_array().expect("list");
        assert_eq!(list.len(), 2);
        let ours = &list[0];
        assert_eq!(ours["id"], "Deep Mine");
        assert_eq!(ours["source"], "Java");
        assert_eq!(ours["javaInstance"]["configId"], "abc");
        assert_eq!(ours["javaInstance"]["game"]["type"], "singleplayer");
        // Чужая запись сохранена.
        assert_eq!(list[1]["id"], "Old");
        assert_eq!(doc["version"], 2);
    }

    #[test]
    fn quick_play_replaces_same_world_instead_of_duplicating() {
        let root = temp_root("qp-replace");
        let mc = make_data_dir(&root);
        std::fs::write(
            mc.join(QUICK_PLAY_FILE),
            r#"{"quickPlayData": {"111": [
                {"epochLastPlayedTimeMs": 1, "id": "Deep Mine", "javaInstance": {"configId": "abc"}, "source": "Java"}]},
                "version": 2}"#,
        )
        .expect("qp");
        add_quick_play(&mc, "abc", "Deep Mine", "Deep Mine", None).expect("add");
        let doc = read_json(&mc.join(QUICK_PLAY_FILE)).expect("doc");
        assert_eq!(doc["quickPlayData"]["111"].as_array().expect("list").len(), 1);
    }

    #[test]
    fn quick_play_without_existing_account_is_rejected() {
        let root = temp_root("qp-empty");
        let mc = make_data_dir(&root);
        std::fs::write(mc.join(QUICK_PLAY_FILE), r#"{"quickPlayData": {}, "version": 2}"#)
            .expect("qp");
        let err = add_quick_play(&mc, "abc", "W", "W", None).expect_err("no remote id");
        assert_eq!(err.code(), "E_NO_DATA_DIR");
    }

    #[test]
    fn quick_play_enabled_reads_settings() {
        let root = temp_root("qp-flag");
        let mc = make_data_dir(&root);
        std::fs::write(
            mc.join(SETTINGS_FILE),
            r#"{"version": 17, "quickPlayEnabled": true}"#,
        )
        .expect("settings");
        assert!(quick_play_enabled(&mc));
    }

    #[test]
    fn icon_requires_readable_png_and_returns_data_uri() {
        let root = temp_root("icon");
        let missing = root.join("nope.png");
        assert!(icon_data_uri(Some(&missing)).is_none());
        assert!(icon_data_uri(None).is_none());

        // Настоящий PNG произвольного размера должен уехать в 128×128.
        let img = image::RgbaImage::from_pixel(64, 32, image::Rgba([10, 20, 30, 255]));
        let png = root.join("icon.png");
        img.save(&png).expect("save");

        let uri = icon_data_uri(Some(&png)).expect("uri");
        assert!(uri.starts_with("data:image/png;base64,"));
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &uri[22..]).expect("b64");
        let back = image::load_from_memory(&raw).expect("png");
        assert_eq!((back.width(), back.height()), (ICON_SIZE, ICON_SIZE));
    }

    #[test]
    fn enumerate_reads_installation_targets() {
        let root = temp_root("targets");
        let mc = make_data_dir(&root);
        let targets = enumerate_targets(&mc);
        let ours = targets.iter().find(|t| t.name == "Моя").expect("ours");
        assert_eq!(ours.launcher, LauncherKind::Official);
        assert_eq!(ours.minecraft_version, "1.21.11");
        assert_eq!(ours.loader, LoaderKind::Vanilla);
        // Последняя стабильная из манифеста — та же 1.21.11, отдельную карточку
        // для неё не заводим: цель уже есть.
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn latest_release_adds_root_target_when_no_installation_matches() {
        let root = temp_root("latest");
        let mc = make_data_dir(&root);
        // Последняя версия — 26.3, её установки у пользователя нет.
        std::fs::write(
            mc.join("versions").join("version_manifest_v2.json"),
            r#"{"latest": {"release": {"id": "26.3", "type": "release"}, "snapshot": {"id": "26.4-snapshot-4", "type": "snapshot"}}}"#,
        )
        .expect("manifest");
        let vdir = mc.join("versions").join("26.3");
        std::fs::create_dir_all(&vdir).expect("vdir");
        std::fs::write(
            vdir.join("26.3.json"),
            r#"{"id": "26.3", "mainClass": "net.minecraft.client.main.Main", "libraries": []}"#,
        )
        .expect("json");

        let targets = enumerate_targets(&mc);
        let root_target = targets
            .iter()
            .find(|t| t.name.starts_with("Minecraft ("))
            .expect("latest target");
        // Ванильная карта без своей установки должна попасть в корень игры.
        assert_eq!(root_target.path, mc);
        assert_eq!(root_target.minecraft_version, "26.3");
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn launch_by_aumid_returns_command_without_spawning() {
        // Команду собираем без запуска: в тесте Windows есть, но открывать
        // окно лаунчера в юнит-тесте не нужно.
        let cmd = format!("shell:AppsFolder\\{STORE_AUMID}");
        assert!(cmd.contains("Microsoft.4297127D64EC6"));
    }

    /// Живая проверка на машине разработчика: каким процессом представляется
    /// Store-сборка и ловит ли его наш матчер. Требует запущенного лаунчера —
    /// иначе печатает, что процессов нет.
    #[test]
    #[ignore = "живая проверка: сначала запустите официальный лаунчер"]
    fn live_dumps_official_processes() {
        use sysinfo::{ProcessesToUpdate, System};
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut found = 0;
        for proc in sys.processes().values() {
            if !proc.name().to_string_lossy().to_lowercase().contains("minecraft") {
                continue;
            }
            found += 1;
            println!("--- pid={}", proc.pid().as_u32());
            println!("    name = {}", proc.name().to_string_lossy());
            println!("    exe  = {:?}", proc.exe());
            println!("    cmd  = {:?}", proc.cmd());
        }
        println!("процессов с именем minecraft: {found}");
        println!(
            "close_launcher(Official) вернул: {}",
            crate::services::detect::close_launcher(LauncherKind::Official)
        );
    }

    /// Живая проверка на машине разработчика: Store-рантайм лежит не в
    /// `<game_dir>/runtime`, а в пакете MSIX, и именно из-за этого его не
    /// находил прежний поиск. `#[ignore]` — результат зависит от того, какая
    /// сборка лаунчера установлена у конкретного человека.
    #[test]
    #[ignore = "живая проверка Store-рантайма, зависит от машины"]
    fn live_resolves_store_java() {
        let roots = java_roots(Path::new("C:/nonexistent/.minecraft"));
        println!("mojang roots: {:?}", roots.mojang);
        let req = java::java_req_for(LoaderKind::NeoForge, "1.21.11");
        println!("требование: {req:?}");
        let resolved = java::resolve_java(&roots, &req);
        println!("выбрана Java: {resolved:?}");
        assert!(resolved.is_ok(), "на этой машине Store-рантайм обязан найтись");
    }
}