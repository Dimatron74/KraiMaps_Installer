//! TLauncher: общий game dir без инстансов.
//!
//! Версии — папки `versions/<Имя>/` (`<Имя>.json` + `<Имя>.jar` + `natives/`,
//! у пользовательских — ещё `TLauncherAdditional.json`). Имя в списке TL =
//! имя папки (= `id` в json), отдельного display name нет.
//! Наши версии помечаем суффиксом ` [KK]` (только созданные нами — чужое
//! не переименовываем). Выбор версии — `login.version.game` в
//! `tlauncher-2.0.properties` (правим только при закрытом TL: лаунчер
//! перезаписывает конфиг при выходе).

use crate::domain::{GameTarget, InstallError, LauncherKind, LoaderKind};
use crate::services::{deploy, java, site};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Суффикс-метка наших версий (`fabric-loader-0.19.5-1.20.1 [KK]`).
pub const MARK_SUFFIX: &str = " [KK]";

/// Маркер недокачанной версии (лежит в корне game dir, пока идёт
/// провижининг; снимается при exit 0). `find_match` такие тройки
/// пропускает — повтор всегда переустанавливает, а не доверяет
/// недописанной папке. Живой кейс: обрыв сети в Forge 1.18.2.
const INCOMPLETE_PREFIX: &str = ".kraimaps-incomplete";

/// Имя маркера для тройки (mc, загрузчик, версия). Только
/// filename-safe символы — остальное в `_`.
fn incomplete_marker_path(
    data_dir: &Path,
    mc: &str,
    loader: LoaderKind,
    loader_version: &str,
) -> PathBuf {
    fn safe(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }
    let kind = match loader {
        LoaderKind::Fabric => "fabric",
        LoaderKind::Forge => "forge",
        LoaderKind::Quilt => "quilt",
        LoaderKind::NeoForge => "neoforge",
        LoaderKind::Vanilla => "vanilla",
    };
    data_dir.join(format!(
        "{INCOMPLETE_PREFIX}-{kind}-{}-{}",
        safe(mc),
        safe(loader_version)
    ))
}

/// Имя properties-файла TLauncher (лежит в `%APPDATA%/.tlauncher/`,
/// НЕ в game dir).
const PROPS_FILE: &str = "tlauncher-2.0.properties";

/// Нужные ключи из `tlauncher-2.0.properties`.
#[derive(Debug, Clone, Default)]
pub struct TlauncherProps {
    pub selected_version: Option<String>,
    pub game_dir: Option<PathBuf>,
}

/// Java-properties unescape для значений (`\:` → `:`, `\\` → `\`,
/// `\uXXXX` → символ). Неизвестные `\x` — как есть без слеша.
fn unescape_java(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if hex.len() == 4 {
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                            continue;
                        }
                    }
                    out.push_str("\\u");
                    out.push_str(&hex);
                } else {
                    out.push('\\');
                    out.push('u');
                    out.push_str(&hex);
                }
            }
            Some(x) => out.push(x),
            None => out.push('\\'),
        }
    }
    out
}

/// Парсинг properties: комментарии `#`/`!`, разделитель первое `=`/`:`.
pub fn parse_tlauncher_properties(text: &str) -> TlauncherProps {
    let mut props = TlauncherProps::default();
    for raw_line in text.lines() {
        let line = raw_line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let split = line.find(['=', ':']).map(|i| line.split_at(i));
        let (key, value) = match split {
            Some((k, v)) => (k.trim(), v[1..].trim()),
            None => continue,
        };
        match key {
            "login.version.game" => {
                let v = unescape_java(value);
                if !v.is_empty() {
                    props.selected_version = Some(v);
                }
            }
            "minecraft.gamedir" => {
                let v = unescape_java(value);
                if !v.is_empty() {
                    props.game_dir = Some(PathBuf::from(v));
                }
            }
            _ => {}
        }
    }
    props
}

/// Новый текст properties с подставленным выбором версии.
/// Существующий ключ заменяется, отсутствующий — дописывается.
pub fn render_properties_with_selected(original: &str, selected: &str) -> String {
    let trailing_nl = original.ends_with('\n');
    let mut lines: Vec<String> = original.lines().map(|l| l.to_string()).collect();
    let mut replaced = false;
    for line in lines.iter_mut() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("login.version.game")
            && trimmed["login.version.game".len()..]
                .trim_start()
                .starts_with(['=', ':'])
        {
            *line = format!("login.version.game={selected}");
            replaced = true;
        }
    }
    if !replaced {
        lines.push(format!("login.version.game={selected}"));
    }
    let mut out = lines.join("\n");
    if trailing_nl {
        out.push('\n');
    }
    out
}

/// Запись выбора версии. `props_dir` — каталог с properties
/// (`%APPDATA%/.tlauncher`). Файла нет, но дом TL на месте — создаём
/// минимальный. Нет даже каталога — TL сюда не ставился, ничего не
/// фабрикуем, возвращаем `false`. Вызывать только при закрытом TL.
pub fn set_selected_version(props_dir: &Path, selected: &str) -> Result<bool, InstallError> {
    let path = props_dir.join(PROPS_FILE);
    if !path.is_file() && !props_dir.is_dir() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    let rendered = render_properties_with_selected(&original, selected);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, rendered)?;
    Ok(true)
}

/// Распознанная версия из `versions/`.
#[derive(Debug, Clone)]
pub struct TlauncherVersion {
    pub dir_name: String,
    pub mc_version: String,
    pub loader: LoaderKind,
    pub loader_version: Option<String>,
}

/// Строгий id ваниллы (`"1.20.1"`, `"1.21"`).
fn is_vanilla_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Ведущий токен версии (`"1.20.1-47.4.20"` → `"1.20.1"`).
fn leading_version(s: &str) -> Option<String> {
    let mut dots = 0u32;
    let mut end = 0usize;
    let mut first_len = 0usize;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
            if dots == 0 {
                first_len = end;
            }
        } else if c == '.' && dots < 2 {
            // Точка — разделитель только между цифровыми группами.
            let rest = &s[i + 1..];
            if rest.chars().next().is_some_and(|n| n.is_ascii_digit()) {
                dots += 1;
                end = i + 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    if first_len == 0 {
        return None;
    }
    if dots == 0 {
        return None; // одиночное число — не версия игры
    }
    Some(s[..end].trim_end_matches('.').to_string())
}

/// Первый токен вида `d.d[.d]` в строке (для id версий).
fn first_version_token(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            if let Some(v) = leading_version(&s[i..]) {
                return Some(v);
            }
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    None
}

/// MC-версия из библиотек: `net/minecraft/client/<MC>-...` (пути) или
/// `net.minecraft:client:<MC>-...` (координаты).
/// Библиотеки без нужных ключей пропускаются (не прерывают поиск —
/// живой кейс: первая же запись без `path`/`name` роняла весь скан).
fn mc_from_libraries(libs: &[serde_json::Value]) -> Option<String> {
    for lib in libs {
        for key in ["path", "name"] {
            let Some(s) = lib.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            for marker in [
                "minecraft/client/",
                "minecraft:client:",
                "minecraft/server/",
                "minecraft:server:",
            ] {
                if let Some(pos) = s.find(marker) {
                    if let Some(v) = leading_version(&s[pos + marker.len()..]) {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// MC-версия из аргументов (`--fml.mcVersion <MC>`).
fn mc_from_args(json: &serde_json::Value) -> Option<String> {
    let game = json.get("arguments")?.get("game")?.as_array()?;
    let mut iter = game.iter();
    while let Some(item) = iter.next() {
        if item.as_str() == Some("--fml.mcVersion") {
            if let Some(v) = iter.next().and_then(|v| v.as_str()) {
                if is_vanilla_id(v) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn libraries_of(json: &serde_json::Value) -> Vec<serde_json::Value> {
    json.get("libraries")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Координата библиотеки (`group:artifact:version...`) по имени артефакта.
fn find_library<'a>(
    libs: &'a [serde_json::Value],
    group_prefix: &str,
    artifacts: &[&str],
) -> Option<(&'a str, &'a str)> {
    for lib in libs {
        // Запись без `name` — не повод ронять весь поиск (см. выше).
        let Some(name) = lib.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut parts = name.split(':');
        let group = parts.next()?;
        let artifact = parts.next()?;
        let version = parts.next()?;
        if group.starts_with(group_prefix) && artifacts.contains(&artifact) {
            return Some((artifact, version));
        }
    }
    None
}

/// Значение аргумента (`--fml.forgeVersion <v>`).
fn arg_value(json: &serde_json::Value, flag: &str) -> Option<String> {
    let game = json.get("arguments")?.get("game")?.as_array()?;
    let mut iter = game.iter();
    while let Some(item) = iter.next() {
        if item.as_str() == Some(flag) {
            return iter
                .next()
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// Загрузчик + его версия из json версии.
fn loader_from_json(json: &serde_json::Value, mc: &str) -> Option<(LoaderKind, Option<String>)> {
    let libs = libraries_of(json);
    // Явный аргумент Forge точнее эвристик по библиотекам.
    if let Some(v) = arg_value(json, "--fml.forgeVersion") {
        return Some((LoaderKind::Forge, Some(v)));
    }
    if let Some((_, v)) = find_library(&libs, "net.minecraftforge", &["forge", "fmlloader"]) {
        let version = v
            .strip_prefix(&format!("{mc}-"))
            .unwrap_or(v)
            .to_string();
        return Some((LoaderKind::Forge, Some(version)));
    }
    if let Some((_, v)) = find_library(&libs, "net.neoforged", &["neoforge"]) {
        return Some((LoaderKind::NeoForge, Some(v.to_string())));
    }
    if let Some((_, v)) = find_library(&libs, "net.fabricmc", &["fabric-loader"]) {
        return Some((LoaderKind::Fabric, Some(v.to_string())));
    }
    if let Some((_, v)) = find_library(&libs, "org.quiltmc", &["quilt-loader"]) {
        return Some((LoaderKind::Quilt, Some(v.to_string())));
    }
    let main = json.get("mainClass").and_then(|m| m.as_str()).unwrap_or("");
    if main.contains("net.fabricmc.loader") {
        return Some((LoaderKind::Fabric, None));
    }
    if main.contains("org.quiltmc.loader") {
        return Some((LoaderKind::Quilt, None));
    }
    if main.contains("cpw.mods.bootstraplauncher") {
        return None; // Forge/NeoForge без библиотек неразличимы — не гадаем
    }
    Some((LoaderKind::Vanilla, None))
}

/// Распознать папку версии. `None` — непонятная версия, пропускаем
/// (строгий фильтр важнее полноты, как у Prism без `net.minecraft`).
pub fn parse_version_json(dir_name: &str, json: &serde_json::Value) -> Option<TlauncherVersion> {
    let mc_version = json
        .get("inheritsFrom")
        .and_then(|v| v.as_str())
        .filter(|v| is_vanilla_id(v))
        .map(|v| v.to_string())
        .or_else(|| mc_from_libraries(&libraries_of(json)))
        .or_else(|| mc_from_args(json))
        .or_else(|| first_version_token(dir_name))?;
    let (loader, loader_version) = loader_from_json(json, &mc_version)?;
    Some(TlauncherVersion {
        dir_name: dir_name.to_string(),
        mc_version,
        loader,
        loader_version,
    })
}

/// Цели TLauncher: по одной на папку версии; `path` — сам game dir
/// (общий для всех версий, инстансов нет). Детерминированный порядок.
pub fn enumerate_tlauncher_targets(data_dir: &Path) -> Vec<GameTarget> {
    let mut out = Vec::new();
    let versions = data_dir.join("versions");
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
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let Some(name) = name else { continue };
        let json_path = dir.join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&json_path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(v) = parse_version_json(&name, &json) {
            out.push(GameTarget {
                launcher: LauncherKind::Tlauncher,
                name: v.dir_name,
                path: data_dir.to_path_buf(),
                minecraft_version: v.mc_version,
                loader: v.loader,
                loader_version: v.loader_version,
            });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Корень данных TLauncher: `versions/` + сигнал TL (`TLauncher*.exe`
/// или `TlauncherProfiles.json`). Голый `versions/` без сигналов — это
/// официальный лаунчер, не наш клиент.
pub fn is_tlauncher_data_dir(p: &Path) -> bool {
    if !p.is_dir() || !p.join("versions").is_dir() {
        return false;
    }
    if p.join("TlauncherProfiles.json").is_file() {
        return true;
    }
    find_tlauncher_exe(p).is_some()
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

/// Запущен ли TLauncher: java-процессы, чей exe лежит под `.tlauncher`
/// (лаунчер; игра — из Mojang-рантайма, её не трогаем).
pub(crate) fn tlauncher_launcher_processes() -> Vec<PathBuf> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut out = Vec::new();
    for proc in sys.processes().values() {
        let name = proc.name().to_string_lossy().to_lowercase();
        if name != "java.exe" && name != "javaw.exe" && name != "java" {
            continue;
        }
        if let Some(exe) = proc.exe().map(|p| p.to_path_buf()) {
            if exe.to_string_lossy().to_lowercase().contains(".tlauncher") {
                out.push(exe);
            }
        }
    }
    out
}

pub(crate) fn tlauncher_running() -> bool {
    !tlauncher_launcher_processes().is_empty()
}

/// Аккуратно закрывает TLauncher перед правкой версий/конфига (иначе
/// лаунчер перезапишет properties при выходе). Как `close_launcher`:
/// просим процессы завершиться, ждём до ~10 с. `false` — не ошибка,
/// установка продолжается, но предвыбор версии пропускается.
pub fn close_tlauncher() -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    for _ in 0..40 {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut alive = false;
        for proc in sys.processes().values() {
            let name = proc.name().to_string_lossy().to_lowercase();
            let is_launcher_java = (name == "java.exe" || name == "javaw.exe" || name == "java")
                && proc
                    .exe()
                    .is_some_and(|e| e.to_string_lossy().to_lowercase().contains(".tlauncher"));
            if is_launcher_java {
                alive = true;
                proc.kill();
            }
        }
        if !alive {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    !tlauncher_running()
}

/// Переименование созданной нами версии в помеченную:
/// папка + `id` в json + jar (если есть). Чужое не трогаем — функция
/// вызывается только для версий из провижининга.
pub fn rename_to_marked(data_dir: &Path, dir_name: &str) -> Result<String, InstallError> {
    let marked = marked_name(dir_name);
    if marked == dir_name {
        return Ok(marked);
    }
    let versions = data_dir.join("versions");
    let src = versions.join(dir_name);
    let dst = versions.join(&marked);
    if !src.is_dir() {
        return Err(InstallError::ProvisionFailed(format!(
            "папка версии пропала: {}",
            src.display()
        )));
    }
    if dst.exists() {
        return Err(InstallError::ProvisionFailed(format!(
            "папка {marked} уже существует — уберите её вручную"
        )));
    }
    std::fs::rename(&src, &dst)?;
    let old_json = dst.join(format!("{dir_name}.json"));
    let new_json = dst.join(format!("{marked}.json"));
    if old_json.is_file() {
        std::fs::rename(&old_json, &new_json)?;
        let text = std::fs::read_to_string(&new_json)?;
        let mut json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| InstallError::ProvisionFailed(format!("json версии повреждён: {e}")))?;
        if let Some(obj) = json.as_object_mut() {
            obj.insert("id".to_string(), serde_json::Value::String(marked.clone()));
        }
        std::fs::write(&new_json, serde_json::to_string_pretty(&json).expect("json"))?;
    }
    let old_jar = dst.join(format!("{dir_name}.jar"));
    if old_jar.is_file() {
        std::fs::rename(&old_jar, dst.join(format!("{marked}.jar")))?;
    }
    Ok(marked)
}

/// Пометка имени версии (идемпотентно).
pub fn marked_name(name: &str) -> String {
    format!("{}{MARK_SUFFIX}", strip_mark(name))
}

/// Имя без пометки (для матчинга и сравнений).
pub fn strip_mark(name: &str) -> String {
    name.strip_suffix(MARK_SUFFIX).unwrap_or(name).to_string()
}

/// Алгоритм сверки установщика (файл чексуммы рядом с jar на том же maven).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgo {
    Sha1,
    Sha256,
}

/// Выбор установщика Fabric: первый stable из meta
/// (`meta.fabricmc.net/v2/versions/installer`, свежие сверху).
pub fn pick_fabric_installer(doc: &serde_json::Value) -> Option<String> {
    doc.as_array()?.iter().find_map(|entry| {
        if entry.get("stable").and_then(|v| v.as_bool()).unwrap_or(false) {
            entry
                .get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
    })
}

/// Выбор установщика Quilt: максимум стабильных (meta без флага stable,
/// формат версий тот же — переиспользуем ключ из `versions`).
pub fn pick_quilt_installer(doc: &serde_json::Value) -> Option<String> {
    doc.as_array()?
        .iter()
        .filter_map(|e| e.get("version")?.as_str())
        .filter(|v| !v.contains('-') && !v.contains('+'))
        .max_by(|a, b| {
            crate::services::versions::version_key(a)
                .cmp(&crate::services::versions::version_key(b))
        })
        .map(|s| s.to_string())
}

/// URL установщика + URL чексуммы на официальном maven.
/// Fabric/Forge отдают `.sha1`, NeoForge/Quilt — `.sha256`
/// (у Quilt сверяем с maven, НЕ с meta API: хэш в meta протухший,
/// факт мини-spike).
pub fn installer_download(
    loader: LoaderKind,
    mc: &str,
    installer_version: &str,
) -> (String, String, ChecksumAlgo) {
    match loader {
        LoaderKind::Fabric => {
            let jar = format!(
                "https://maven.fabricmc.net/net/fabricmc/fabric-installer/{installer_version}/fabric-installer-{installer_version}.jar"
            );
            (jar.clone(), format!("{jar}.sha1"), ChecksumAlgo::Sha1)
        }
        LoaderKind::Forge => {
            let ver = format!("{mc}-{installer_version}");
            let jar = format!(
                "https://maven.minecraftforge.net/net/minecraftforge/forge/{ver}/forge-{ver}-installer.jar"
            );
            (jar.clone(), format!("{jar}.sha1"), ChecksumAlgo::Sha1)
        }
        LoaderKind::NeoForge => {
            let jar = format!(
                "https://maven.neoforged.net/releases/net/neoforged/neoforge/{installer_version}/neoforge-{installer_version}-installer.jar"
            );
            (jar.clone(), format!("{jar}.sha256"), ChecksumAlgo::Sha256)
        }
        LoaderKind::Quilt => {
            let jar = format!(
                "https://maven.quiltmc.org/repository/release/org/quiltmc/quilt-installer/{installer_version}/quilt-installer-{installer_version}.jar"
            );
            (jar.clone(), format!("{jar}.sha256"), ChecksumAlgo::Sha256)
        }
        LoaderKind::Vanilla => {
            unreachable!("у ваниллы нет установщика — провижининг не вызывается")
        }
    }
}

/// Итог подготовки версии для экрана Deploy/Done.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnsureResult {
    pub target: GameTarget,
    /// Общие `mods/` пусты — wipe не потребуется.
    pub mods_empty: bool,
    /// Предвыбор версии в конфиге TL выполнен.
    pub preselected: bool,
}

async fn get_ok(
    client: &reqwest::Client,
    url: &str,
    ctx: &str,
) -> Result<reqwest::Response, InstallError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::ProvisionFailed(format!("{ctx}: {e}")))?;
    if !resp.status().is_success() {
        return Err(InstallError::ProvisionFailed(format!(
            "{ctx}: http {}",
            resp.status()
        )));
    }
    Ok(resp)
}

fn verify_checksum(
    path: &Path,
    expected_hex: &str,
    algo: ChecksumAlgo,
) -> Result<(), InstallError> {
    let bytes = std::fs::read(path)?;
    let digest: Vec<u8> = match algo {
        ChecksumAlgo::Sha1 => {
            use sha1::Digest;
            sha1::Sha1::digest(&bytes).to_vec()
        }
        ChecksumAlgo::Sha256 => {
            use sha2::Digest;
            sha2::Sha256::digest(&bytes).to_vec()
        }
    };
    let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    if actual != expected_hex.trim().to_lowercase() {
        return Err(InstallError::HashMismatch {
            expected: expected_hex.trim().to_string(),
            actual,
        });
    }
    Ok(())
}

/// Скачивание установщика с прогрессом + сверка чексуммы.
async fn download_installer(
    client: &reqwest::Client,
    jar_url: &str,
    sum_url: &str,
    algo: ChecksumAlgo,
    dest: &Path,
    progress: deploy::Progress<'_>,
) -> Result<(), InstallError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let resp = get_ok(client, jar_url, "скачивание установщика").await?;
    let total = resp.content_length().unwrap_or(0);
    let mut done = 0u64;
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(InstallError::from)?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| InstallError::ProvisionFailed(format!("скачивание установщика: {e}")))?;
        done += chunk.len() as u64;
        file.write_all(&chunk)
            .await
            .map_err(InstallError::from)?;
        progress(deploy::StageEvent::new(
            "installer-download",
            done,
            total,
            "installer.jar".to_string(),
        ));
    }
    file.flush().await.map_err(InstallError::from)?;
    drop(file);
    let sum_text = get_ok(client, sum_url, "контрольная сумма")
        .await?
        .text()
        .await
        .map_err(|e| InstallError::ProvisionFailed(format!("контрольная сумма: {e}")))?;
    let expected = sum_text.split_whitespace().next().unwrap_or("").to_string();
    if expected.is_empty() {
        return Err(InstallError::ProvisionFailed(
            "пустая контрольная сумма на maven".to_string(),
        ));
    }
    verify_checksum(dest, &expected, algo)
}

/// Аргументы установщиков (зафиксированы мини-spike, только официалы).
fn installer_args(
    loader: LoaderKind,
    mc: &str,
    loader_version: &str,
    data_dir: &Path,
) -> Vec<String> {
    let dir = data_dir.to_string_lossy().into_owned();
    match loader {
        LoaderKind::Fabric => vec![
            "client".to_string(),
            "-mcversion".to_string(),
            mc.to_string(),
            "-loader".to_string(),
            loader_version.to_string(),
            "-dir".to_string(),
            dir,
            "-noprofile".to_string(),
        ],
        LoaderKind::Forge => vec!["--installClient".to_string(), dir],
        LoaderKind::NeoForge => vec!["--install-client".to_string(), dir],
        LoaderKind::Quilt => vec![
            "install".to_string(),
            "client".to_string(),
            mc.to_string(),
            loader_version.to_string(),
            format!("--install-dir={dir}"),
            "--no-profile".to_string(),
        ],
        LoaderKind::Vanilla => {
            unreachable!("у ваниллы нет установщика — провижининг не вызывается")
        }
    }
}

/// Лимиты прогона установщика: тишина в логе дольше 10 минут — считаем
/// зависшим (убиваем); общий потолок 30 минут на медленные сети.
const SILENCE_TIMEOUT_SECS: u64 = 600;
const TOTAL_TIMEOUT_SECS: u64 = 1800;
/// Попыток прогона: transient-обрывы сети (живой кейс Forge 1.18.2 —
/// TLS-рвение на одной библиотеке из ~150) переживаем молча, как просит
/// сам установщик («These libraries failed to download. Try again»).
const INSTALL_ATTEMPTS: u32 = 3;
/// Потолок лога в памяти (старые строки вытесняются).
const LOG_CAP_CHARS: usize = 262_144;

/// JVM-флаги установщика. Headless обязателен: невидимый GUI-диалог
/// старого инсталлера иначе ждёт клика вечно, а так — быстрая ошибка.
pub fn jvm_args() -> Vec<String> {
    vec!["-Djava.awt.headless=true".to_string()]
}

/// N попыток асинхронной операции (первая ошибка не фатальна).
async fn retry_async<F, Fut>(tries: u32, mut op: F) -> Result<String, InstallError>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<String, InstallError>>,
{
    let mut last_err = None;
    for attempt in 1..=tries.max(1) {
        match op(attempt).await {
            Ok(out) => return Ok(out),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("at least one attempt"))
}

/// Прогон произвольной команды с живым стримингом stdout/stderr в прогресс
/// и сторожевым таймаутом по тишине. Возвращает хвост лога; неуспешный
/// exit — ошибка с хвостом для «Подробнее».
async fn run_command_with_limits(
    exe: &Path,
    args: &[String],
    progress: deploy::Progress<'_>,
    silence_timeout: Duration,
    total_timeout: Duration,
) -> Result<String, InstallError> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    progress(deploy::StageEvent::new(
        "installer-run",
        0,
        0,
        exe.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ));
    let mut child = tokio::process::Command::new(exe)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Ввод не читаем никогда: установщик не должен ждать клавишу.
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| InstallError::ProvisionFailed(format!("запуск установщика: {e}")))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    {
        use tokio::io::AsyncRead;
        let mut readers: Vec<Box<dyn AsyncRead + Unpin + Send>> = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            readers.push(Box::new(pipe));
        }
        if let Some(pipe) = child.stderr.take() {
            readers.push(Box::new(pipe));
        }
        for reader in readers {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
    }
    drop(tx);

    let start = std::time::Instant::now();
    let mut last_output = start;
    let mut log = String::new();
    let mut line_no = 0u64;
    loop {
        let silence_left = silence_timeout.checked_sub(last_output.elapsed());
        let total_left = total_timeout.checked_sub(start.elapsed());
        let wait_for = match (silence_left, total_left) {
            (_, None) => None, // общий лимит исчерпан
            (None, _) => None, // тишина дольше лимита
            (Some(s), Some(t)) => Some(s.min(t)),
        };
        let Some(wait_for) = wait_for else {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let total_out = total_left.is_none();
            return Err(InstallError::ProvisionFailed(if total_out {
                "установщик превысил общий лимит 30 минут".to_string()
            } else {
                "установщик молчит больше 10 минут — похоже, завис".to_string()
            }));
        };
        match tokio::time::timeout(wait_for, rx.recv()).await {
            Ok(Some(line)) => {
                last_output = std::time::Instant::now();
                line_no += 1;
                log.push_str(line.trim_end());
                log.push('\n');
                if log.len() > LOG_CAP_CHARS {
                    let cut = log.len() - LOG_CAP_CHARS * 3 / 4;
                    if let Some(at) = log.char_indices().map(|(i, _)| i).find(|&i| i >= cut) {
                        log.drain(..at);
                    }
                }
                let shown: String = line.chars().take(200).collect();
                progress(deploy::StageEvent::new(
                    "installer-run",
                    line_no,
                    0,
                    shown,
                ));
            }
            Ok(None) => break, // оба пайпа закрыты
            Err(_) => continue, // тихий интервал вышел — пересчитаем таймеры
        }
    }
    // Вывод закрыт; ждём выход в остатке общего лимита.
    let remain = total_timeout
        .checked_sub(start.elapsed())
        .unwrap_or(Duration::from_secs(1));
    let status = tokio::time::timeout(remain.max(Duration::from_secs(1)), child.wait())
        .await
        .map_err(|_| {
            InstallError::ProvisionFailed(
                "установщик не завершился после закрытия вывода".to_string(),
            )
        })?
        .map_err(|e| InstallError::ProvisionFailed(format!("установщик: {e}")))?;
    let tail = tail_chars(&log, 2000);
    if !status.success() {
        return Err(InstallError::ProvisionFailed(format!(
            "установщик завершился с ошибкой: {tail}"
        )));
    }
    Ok(tail)
}

/// Последние `limit` символов (хвост лога для «Подробнее»).
fn tail_chars(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let cut = s.len() - limit;
    match s.char_indices().map(|(i, _)| i).find(|&i| i >= cut) {
        Some(at) => s[at..].to_string(),
        None => s.to_string(),
    }
}

/// Прогон установщика загрузчика: headless JVM + до 3 попыток.
/// Возвращает хвост лога для «Подробнее».
pub async fn run_installer(
    java: &Path,
    jar: &Path,
    args: &[String],
    progress: deploy::Progress<'_>,
) -> Result<String, InstallError> {
    let mut full = jvm_args();
    full.push("-jar".to_string());
    full.push(jar.to_string_lossy().into_owned());
    full.extend(args.iter().cloned());
    retry_async(INSTALL_ATTEMPTS, |attempt| {
        if attempt > 1 {
            progress(deploy::StageEvent::new(
                "installer-run",
                0,
                0,
                format!("Попытка {attempt} из {INSTALL_ATTEMPTS}…"),
            ));
        }
        run_command_with_limits(
            java,
            &full,
            progress,
            Duration::from_secs(SILENCE_TIMEOUT_SECS),
            Duration::from_secs(TOTAL_TIMEOUT_SECS),
        )
    })
    .await
}

/// Forge/NeoForge требуют `launcher_profiles.json` в цели
/// («you need to run the launcher first»). У TL он есть всегда после
/// первого запуска; для свежего каталога создаём минимальный.
fn ensure_launcher_profiles(data_dir: &Path) -> Result<(), InstallError> {
    for name in [
        "launcher_profiles.json",
        "launcher_profiles_microsoft_store.json",
    ] {
        if data_dir.join(name).is_file() {
            return Ok(());
        }
    }
    std::fs::write(data_dir.join("launcher_profiles.json"), "{\"profiles\": {}}")?;
    Ok(())
}

/// Поиск версии по паре (mc, загрузчик, точная версия).
/// Тройку с маркером недокачанной пропускаем (см. выше).
fn find_match(
    data_dir: &Path,
    mc: &str,
    loader: LoaderKind,
    loader_version: &str,
) -> Option<TlauncherVersion> {
    if incomplete_marker_path(data_dir, mc, loader, loader_version).exists() {
        return None;
    }
    let versions = data_dir.join("versions");
    let entries = std::fs::read_dir(&versions).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        // Папка без имени или без json (напр. ванильный `1.18.2/` с одним
        // jar) — пропускаем, а не прерываем сканирование: она сортируется
        // раньше forge-папки и раньше роняла весь поиск (живой кейс).
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{name}.json"))) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(v) = parse_version_json(&name, &json) {
            if v.mc_version == mc
                && v.loader == loader
                && v.loader_version.as_deref() == Some(loader_version)
            {
                return Some(v);
            }
        }
    }
    None
}

fn mods_empty(data_dir: &Path) -> bool {
    std::fs::read_dir(data_dir.join("mods"))
        .map(|mut r| r.next().is_none())
        .unwrap_or(true)
}

/// Финал ensure: цель + предвыбор версии (только при закрытом TL) +
/// флаг пустоты `mods/`.
fn finish_ensure(
    data_dir: &Path,
    roaming: Option<&Path>,
    dir_name: &str,
    mc: &str,
    loader: LoaderKind,
    loader_version: String,
) -> Result<EnsureResult, InstallError> {
    let mut preselected = false;
    if let Some(roaming) = roaming {
        // TL перезаписывает properties при выходе — правим только
        // когда лаунчер точно закрыт, иначе молча пропускаем.
        if !tlauncher_running() {
            preselected =
                set_selected_version(&roaming.join(".tlauncher"), dir_name).unwrap_or(false);
        }
    }
    Ok(EnsureResult {
        target: GameTarget {
            launcher: LauncherKind::Tlauncher,
            name: dir_name.to_string(),
            path: data_dir.to_path_buf(),
            minecraft_version: mc.to_string(),
            loader,
            loader_version: Some(loader_version),
        },
        mods_empty: mods_empty(data_dir),
        preselected,
    })
}

/// Подготовка версии: поиск → (закрыть TL → Java → скачать установщик →
/// прогнать → пометить [KK]) → предвыбор. Долгая операция, прогресс —
/// событиями `provision-progress`.
pub async fn ensure_version(
    data_dir: &Path,
    roaming: Option<&Path>,
    mc_version: &str,
    loader: LoaderKind,
    loader_version: Option<&str>,
    progress: deploy::Progress<'_>,
) -> Result<EnsureResult, InstallError> {
    if mc_version.trim().is_empty() {
        return Err(InstallError::UnknownLoader(
            "неизвестна версия игры для подготовки".to_string(),
        ));
    }
    if loader == LoaderKind::Vanilla {
        return Err(InstallError::UnknownLoader(
            "ванильная версия не требует подготовки".to_string(),
        ));
    }
    // Защита в глубину, как у Prism/Modrinth: диапазон вместо точной
    // версии роняет запуск — такие значения не используем никогда.
    let loader_version = match loader_version {
        Some(v) if crate::services::versions::is_exact_version(v) => v.trim().to_string(),
        _ => {
            return Err(InstallError::UnknownLoader(
                "нет точной версии загрузчика для подготовки версии".to_string(),
            ))
        }
    };

    // Версия уже есть (помеченная или обычная — матчим по паре).
    if let Some(found) = find_match(data_dir, mc_version, loader, &loader_version) {
        let _closed = close_tlauncher();
        return finish_ensure(
            data_dir,
            roaming,
            &found.dir_name,
            mc_version,
            loader,
            loader_version,
        );
    }

    let _closed = close_tlauncher();

    let req = java::java_req_for(loader, mc_version);
    let java_exe = java::resolve_java(data_dir, roaming, &req)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(site::installer_user_agent())
        .build()
        .map_err(|e| InstallError::ProvisionFailed(format!("http client: {e}")))?;
    let installer_version = match loader {
        LoaderKind::Fabric => {
            let doc: serde_json::Value = get_ok(
                &client,
                "https://meta.fabricmc.net/v2/versions/installer",
                "meta fabric",
            )
            .await?
            .json()
            .await
            .map_err(|e| InstallError::ProvisionFailed(format!("meta fabric: {e}")))?;
            pick_fabric_installer(&doc)
                .ok_or_else(|| InstallError::ProvisionFailed("нет установщика Fabric в meta".to_string()))?
        }
        LoaderKind::Quilt => {
            let doc: serde_json::Value = get_ok(
                &client,
                "https://meta.quiltmc.org/v3/versions/installer",
                "meta quilt",
            )
            .await?
            .json()
            .await
            .map_err(|e| InstallError::ProvisionFailed(format!("meta quilt: {e}")))?;
            pick_quilt_installer(&doc)
                .ok_or_else(|| InstallError::ProvisionFailed("нет установщика Quilt в meta".to_string()))?
        }
        LoaderKind::Forge | LoaderKind::NeoForge => loader_version.clone(),
        LoaderKind::Vanilla => {
            unreachable!("ванильная версия не требует подготовки")
        }
    };

    let (jar_url, sum_url, algo) = installer_download(loader, mc_version, &installer_version);
    // Маркер недокачанной версии — до любых записей (см. INCOMPLETE_PREFIX).
    let incomplete = incomplete_marker_path(data_dir, mc_version, loader, &loader_version);
    std::fs::write(&incomplete, b"provisioning")?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let workdir =
        std::env::temp_dir().join(format!("mapinstaller-tl-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&workdir)?;
    let jar_path = workdir.join("installer.jar");
    download_installer(&client, &jar_url, &sum_url, algo, &jar_path, progress).await?;
    if matches!(loader, LoaderKind::Forge | LoaderKind::NeoForge) {
        ensure_launcher_profiles(data_dir)?;
    }
    let args = installer_args(loader, mc_version, &loader_version, data_dir);
    let run_result = run_installer(&java_exe, &jar_path, &args, progress).await;
    // Времянка установщика больше не нужна в любом исходе: состояние
    // продолжения живёт в game dir (библиотеки докачиваются поверх).
    std::fs::remove_dir_all(&workdir).ok();
    let run_tail = run_result?;
    // Успех: снимаем маркер до рескана, иначе find_match нас же пропустит.
    std::fs::remove_file(&incomplete).ok();
    progress(deploy::StageEvent::new(
        "version-finalize",
        1,
        1,
        "installer.jar".to_string(),
    ));
    std::fs::remove_dir_all(&workdir).ok();

    let found = find_match(data_dir, mc_version, loader, &loader_version).ok_or_else(|| {
        InstallError::ProvisionFailed(format!(
            "версия не появилась после установки. Лог установщика: {run_tail}"
        ))
    })?;
    let marked = rename_to_marked(data_dir, &found.dir_name)?;
    finish_ensure(
        data_dir,
        roaming,
        &marked,
        mc_version,
        loader,
        loader_version,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fabric_json() -> serde_json::Value {
        serde_json::json!({
            "inheritsFrom": "1.20.1",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                {"name": "org.ow2.asm:asm:9.10.1", "url": "https://maven.fabricmc.net/"},
                {"name": "net.fabricmc:sponge-mixin:0.17.4+mixin.0.8.7", "url": "https://maven.fabricmc.net/"},
                {"name": "net.fabricmc:intermediary:1.20.1", "url": "https://maven.fabricmc.net/"},
                {"name": "net.fabricmc:fabric-loader:0.19.5", "url": "https://maven.fabricmc.net/"}
            ],
            "id": "fabric-loader-0.19.5-1.20.1",
            "type": "release"
        })
    }

    fn forge_tl_json() -> serde_json::Value {
        serde_json::json!({
            "id": "ForgeOptiFine 1.20.1",
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "arguments": {"game": ["--fml.forgeVersion", "47.4.20", "--fml.mcVersion", "1.20.1"]},
            "libraries": [
                {"name": "net.minecraftforge:fmlloader:1.20.1-47.4.20"},
                {"path": "libraries/net/minecraft/client/1.20.1-20230612.114412/client-extra.jar"}
            ],
            "type": "modified"
        })
    }

    fn neoforge_json() -> serde_json::Value {
        serde_json::json!({
            "id": "1.21.1-neoforge-21.1.148",
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "libraries": [
                {"name": "net.neoforged:neoforge:21.1.148"}
            ],
            "type": "release"
        })
    }

    fn quilt_json() -> serde_json::Value {
        serde_json::json!({
            "inheritsFrom": "1.20.1",
            "mainClass": "org.quiltmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                {"name": "org.quiltmc:quilt-loader:0.26.4", "url": "https://maven.quiltmc.org/"}
            ],
            "id": "quilt-loader-0.26.4-1.20.1",
            "type": "release"
        })
    }

    fn vanilla_json() -> serde_json::Value {
        serde_json::json!({
            "id": "1.21.11",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": [{"name": "com.mojang:patchy:2.2.10"}],
            "type": "release"
        })
    }

    #[test]
    fn fabric_version_parses_mc_and_loader() {
        let v = parse_version_json("fabric-loader-0.19.5-1.20.1", &fabric_json()).expect("parsed");
        assert_eq!(v.mc_version, "1.20.1");
        assert_eq!(v.loader, LoaderKind::Fabric);
        assert_eq!(v.loader_version.as_deref(), Some("0.19.5"));
    }

    #[test]
    fn forge_tl_version_parses_mc_and_loader() {
        let v = parse_version_json("ForgeOptiFine 1.20.1", &forge_tl_json()).expect("parsed");
        assert_eq!(v.mc_version, "1.20.1");
        assert_eq!(v.loader, LoaderKind::Forge);
        assert_eq!(v.loader_version.as_deref(), Some("47.4.20"));
    }

    #[test]
    fn neoforge_version_parses_mc_and_loader() {
        let v =
            parse_version_json("1.21.1-neoforge-21.1.148", &neoforge_json()).expect("parsed");
        assert_eq!(v.mc_version, "1.21.1");
        assert_eq!(v.loader, LoaderKind::NeoForge);
        assert_eq!(v.loader_version.as_deref(), Some("21.1.148"));
    }

    #[test]
    fn quilt_version_parses_mc_and_loader() {
        let v =
            parse_version_json("quilt-loader-0.26.4-1.20.1", &quilt_json()).expect("parsed");
        assert_eq!(v.mc_version, "1.20.1");
        assert_eq!(v.loader, LoaderKind::Quilt);
        assert_eq!(v.loader_version.as_deref(), Some("0.26.4"));
    }

    #[test]
    fn vanilla_version_parses_without_loader() {
        let v = parse_version_json("1.21.11", &vanilla_json()).expect("parsed");
        assert_eq!(v.mc_version, "1.21.11");
        assert_eq!(v.loader, LoaderKind::Vanilla);
        assert_eq!(v.loader_version, None);
    }

    #[test]
    fn version_without_mc_is_skipped() {
        let json = serde_json::json!({
            "id": "Mystery",
            "mainClass": "net.minecraft.client.main.Main",
            "libraries": []
        });
        assert!(parse_version_json("Mystery", &json).is_none());
    }

    #[test]
    fn mark_is_appended_once_and_stripped() {
        assert_eq!(marked_name("Forge 1.20.1"), "Forge 1.20.1 [KK]");
        assert_eq!(marked_name("Forge 1.20.1 [KK]"), "Forge 1.20.1 [KK]");
        assert_eq!(strip_mark("Forge 1.20.1 [KK]"), "Forge 1.20.1");
        assert_eq!(strip_mark("Forge 1.20.1"), "Forge 1.20.1");
    }

    #[test]
    fn properties_parse_selected_and_gamedir() {
        let text = "# TLauncher configuration file\nlogin.version.game=ForgeOptiFine 1.20.1\nminecraft.gamedir=C\\:\\\\Users\\\\X\\\\AppData\\\\Roaming\\\\.minecraft\nlocale1=ru\n";
        let props = parse_tlauncher_properties(text);
        assert_eq!(
            props.selected_version.as_deref(),
            Some("ForgeOptiFine 1.20.1")
        );
        assert_eq!(
            props.game_dir,
            Some(PathBuf::from("C:\\Users\\X\\AppData\\Roaming\\.minecraft"))
        );
    }

    #[test]
    fn properties_unicode_escapes_decode() {
        let text = "modpack.combobox.name=\\u041D\\u0435 \\u0432\\u044B\\u0431\\u0440\\u0430\\u043D\\u043E\nlogin.version.game=1.21.11\n";
        let props = parse_tlauncher_properties(text);
        assert_eq!(props.selected_version.as_deref(), Some("1.21.11"));
    }

    #[test]
    fn render_replaces_selected_and_keeps_rest() {
        let original = "locale1=ru\nlogin.version.game=Old Name\nminecraft.gamedir=C\\:/game\n";
        let rendered = render_properties_with_selected(original, "New Name [KK]");
        assert!(rendered.contains("login.version.game=New Name [KK]\n"));
        assert!(rendered.contains("locale1=ru\n"));
        assert!(!rendered.contains("Old Name"));
    }

    #[test]
    fn render_adds_selected_when_missing() {
        let rendered = render_properties_with_selected("locale1=ru\n", "1.21.11");
        assert!(rendered.contains("login.version.game=1.21.11\n"));
    }

    #[test]
    fn fabric_installer_picks_latest_stable() {
        let doc = serde_json::json!([
            {"version": "1.1.2", "stable": true},
            {"version": "1.1.1", "stable": false},
            {"version": "0.11.2", "stable": false}
        ]);
        assert_eq!(pick_fabric_installer(&doc).as_deref(), Some("1.1.2"));
        let empty = serde_json::json!([]);
        assert_eq!(pick_fabric_installer(&empty), None);
    }

    #[test]
    fn quilt_installer_picks_max_stable() {
        let doc = serde_json::json!([
            {"version": "0.15.1"},
            {"version": "0.15.0"},
            {"version": "0.14.0-beta.1"}
        ]);
        assert_eq!(pick_quilt_installer(&doc).as_deref(), Some("0.15.1"));
    }

    #[test]
    fn installer_urls_point_to_official_mavens() {
        let (jar, sum, algo) = installer_download(LoaderKind::Fabric, "1.20.1", "1.1.2");
        assert_eq!(
            jar,
            "https://maven.fabricmc.net/net/fabricmc/fabric-installer/1.1.2/fabric-installer-1.1.2.jar"
        );
        assert!(sum.ends_with(".sha1"));
        assert_eq!(algo, ChecksumAlgo::Sha1);

        let (jar, sum, algo) = installer_download(LoaderKind::Forge, "1.20.1", "47.4.10");
        assert_eq!(
            jar,
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.4.10/forge-1.20.1-47.4.10-installer.jar"
        );
        assert!(sum.ends_with(".sha1"));
        assert_eq!(algo, ChecksumAlgo::Sha1);
    }

    #[test]
    fn neoforge_installer_url_uses_loader_version() {
        let (jar, sum, algo) = installer_download(LoaderKind::NeoForge, "1.21.1", "21.1.148");
        assert_eq!(
            jar,
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.1.148/neoforge-21.1.148-installer.jar"
        );
        assert!(sum.ends_with(".sha256"));
        assert_eq!(algo, ChecksumAlgo::Sha256);
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
    fn enumerate_finds_versions_and_skips_broken() {
        let root = temp_root("enum");
        let versions = root.join("versions");
        let good = versions.join("fabric-loader-0.19.5-1.20.1");
        std::fs::create_dir_all(&good).expect("good");
        std::fs::write(
            good.join("fabric-loader-0.19.5-1.20.1.json"),
            serde_json::to_string(&fabric_json()).expect("json"),
        )
        .expect("write");
        // Битая версия без json — пропускается молча.
        std::fs::create_dir_all(versions.join("Broken")).expect("broken");

        let targets = enumerate_tlauncher_targets(&root);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].launcher, LauncherKind::Tlauncher);
        assert_eq!(targets[0].name, "fabric-loader-0.19.5-1.20.1");
        assert_eq!(targets[0].path, root);
        assert_eq!(targets[0].minecraft_version, "1.20.1");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn selected_version_roundtrips_through_file() {
        let root = temp_root("props");
        let props = root.join("tlauncher-2.0.properties");
        std::fs::write(&props, "locale1=ru\nlogin.version.game=Old\n").expect("write");
        assert!(set_selected_version(&root, "New [KK]").expect("set"));
        let back = std::fs::read_to_string(&props).expect("read");
        assert!(back.contains("login.version.game=New [KK]"));
        assert!(back.contains("locale1=ru"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_tl_home_is_not_fabricated() {
        // Нет ни файла, ни каталога .tlauncher — TL сюда не ставился:
        // ничего не создаём, честно возвращаем false.
        let root = temp_root("props-missing");
        let missing = root.join("no-such-home");
        assert!(!missing.exists());
        assert_eq!(
            set_selected_version(&missing, "1.21.11").expect("set"),
            false
        );
        assert!(!missing.exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rename_to_marked_moves_folder_json_and_jar() {
        let root = temp_root("rename");
        let versions = root.join("versions");
        let old = versions.join("Forge 1.20.1");
        std::fs::create_dir_all(&old).expect("old");
        std::fs::write(
            old.join("Forge 1.20.1.json"),
            r#"{"id": "Forge 1.20.1", "type": "modified"}"#,
        )
        .expect("json");
        std::fs::write(old.join("Forge 1.20.1.jar"), b"fake-jar").expect("jar");

        let marked = rename_to_marked(&root, "Forge 1.20.1").expect("rename");
        assert_eq!(marked, "Forge 1.20.1 [KK]");
        assert!(!versions.join("Forge 1.20.1").exists());
        let new_dir = versions.join("Forge 1.20.1 [KK]");
        assert!(new_dir.join("Forge 1.20.1 [KK].jar").is_file());
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(new_dir.join("Forge 1.20.1 [KK].json")).expect("read"),
        )
        .expect("json");
        assert_eq!(json["id"], "Forge 1.20.1 [KK]");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn jvm_args_force_headless() {
        // Установщик никогда не должен ждать невидимого GUI-диалога:
        // вместо вечного ожидания — быстрая понятная ошибка.
        assert!(jvm_args().iter().any(|a| a == "-Djava.awt.headless=true"));
    }

    #[test]
    fn installer_args_cover_all_loaders() {
        assert_eq!(
            installer_args(LoaderKind::Fabric, "1.20.1", "0.19.5", Path::new("C:\\game")),
            vec![
                "client",
                "-mcversion",
                "1.20.1",
                "-loader",
                "0.19.5",
                "-dir",
                "C:\\game",
                "-noprofile"
            ]
        );
        assert_eq!(
            installer_args(LoaderKind::Forge, "1.20.1", "47.4.10", Path::new("C:\\game")),
            vec!["--installClient", "C:\\game"]
        );
        assert_eq!(
            installer_args(LoaderKind::NeoForge, "1.21.1", "21.1.148", Path::new("C:\\game")),
            vec!["--install-client", "C:\\game"]
        );
        assert_eq!(
            installer_args(LoaderKind::Quilt, "1.20.1", "0.26.4", Path::new("C:\\game")),
            vec![
                "install",
                "client",
                "1.20.1",
                "0.26.4",
                "--install-dir=C:\\game",
                "--no-profile"
            ]
        );
    }

    #[test]
    fn find_match_skips_version_without_json() {
        // Живой кейс Forge 1.18.2: `versions/1.18.2/` содержит только jar
        // без json и сортируется ПЕРЕД forge-папкой. Сканирование обязано
        // её пропустить, а не прерываться.
        let root = temp_root("nojson");
        let versions = root.join("versions");
        let vanilla = versions.join("1.18.2");
        std::fs::create_dir_all(&vanilla).expect("vanilla");
        std::fs::write(vanilla.join("1.18.2.jar"), b"fake-jar").expect("jar");
        let forge = versions.join("1.18.2-forge-40.3.0");
        std::fs::create_dir_all(&forge).expect("forge");
        std::fs::write(
            forge.join("1.18.2-forge-40.3.0.json"),
            r#"{"id": "1.18.2-forge-40.3.0",
                "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "arguments": {"game": ["--fml.forgeVersion", "40.3.0"]},
                "libraries": [{"name": "net.minecraftforge:forge:1.18.2-40.3.0"}]}"#,
        )
        .expect("json");

        let found = find_match(&root, "1.18.2", LoaderKind::Forge, "40.3.0").expect("match");
        assert_eq!(found.dir_name, "1.18.2-forge-40.3.0");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mc_from_libraries_ignores_libs_without_path_or_name() {
        let libs = vec![
            serde_json::json!({"downloads": {"artifact": {"path": "x"}}}),
            serde_json::json!({"name": "other:lib:1.0"}),
            serde_json::json!({"path": "libraries/net/minecraft/client/1.20.1-abc/client-extra.jar"}),
        ];
        assert_eq!(mc_from_libraries(&libs).as_deref(), Some("1.20.1"));
    }

    #[test]
    fn find_library_ignores_nameless_entries() {
        let libs = vec![
            serde_json::json!({"downloads": {}}),
            serde_json::json!({"name": "net.minecraftforge:forge:1.18.2-40.3.0"}),
        ];
        assert_eq!(
            find_library(&libs, "net.minecraftforge", &["forge", "fmlloader"]),
            Some(("forge", "1.18.2-40.3.0"))
        );
    }

    #[test]
    fn incomplete_marker_hides_partial_version() {        let root = temp_root("incomplete");
        let versions = root.join("versions");
        let vdir = versions.join("1.18.2-forge-40.3.0");
        std::fs::create_dir_all(&vdir).expect("vdir");
        std::fs::write(
            vdir.join("1.18.2-forge-40.3.0.json"),
            r#"{"id": "1.18.2-forge-40.3.0",
                "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
                "arguments": {"game": ["--fml.forgeVersion", "40.3.0"]},
                "libraries": [{"name": "net.minecraftforge:forge:1.18.2-40.3.0"}]}"#,
        )
        .expect("json");
        // Без маркера — готовая версия, матчится.
        assert!(find_match(&root, "1.18.2", LoaderKind::Forge, "40.3.0").is_some());
        // С маркером недокачанной — пропускается, уйдёт в переустановку.
        std::fs::write(
            incomplete_marker_path(&root, "1.18.2", LoaderKind::Forge, "40.3.0"),
            b"",
        )
        .expect("marker");
        assert!(find_match(&root, "1.18.2", LoaderKind::Forge, "40.3.0").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn retry_async_retries_then_succeeds() {
        use std::sync::{Arc, Mutex};
        let attempts = Arc::new(Mutex::new(0u32));
        let congestion = Arc::clone(&attempts);
        let r = retry_async(3, |_| {
            let congestion = Arc::clone(&congestion);
            async move {
                let mut n = congestion.lock().unwrap();
                *n += 1;
                if *n < 3 {
                    Err(InstallError::ProvisionFailed("boom".to_string()))
                } else {
                    Ok("done".to_string())
                }
            }
        })
        .await;
        assert_eq!(r.unwrap(), "done");
        assert_eq!(*attempts.lock().unwrap(), 3);
    }

    #[tokio::test]
    async fn retry_async_gives_up_after_tries() {
        let r = retry_async(2, |_| async {
            Err::<String, _>(InstallError::ProvisionFailed("x".to_string()))
        })
        .await;
        assert!(r.is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_command_streams_lines_and_reports_tail() {
        use std::sync::{Arc, Mutex};
        let shown = Arc::new(Mutex::new(Vec::<String>::new()));
        let shown_in = Arc::clone(&shown);
        let r = run_command_with_limits(
            Path::new("cmd.exe"),
            &[
                "/C".to_string(),
                "echo out-one & echo err-two 1>&2 & exit 1".to_string(),
            ],
            &|ev: deploy::StageEvent| {
                shown_in.lock().unwrap().push(ev.current_file.clone());
            },
            Duration::from_secs(30),
            Duration::from_secs(60),
        )
        .await;
        let err = r.expect_err("failing exit must error");
        assert!(
            err.details().contains("out-one"),
            "tail must surface, got: {}",
            err.details()
        );
        let shown = shown.lock().unwrap();
        assert!(shown.iter().any(|l| l.contains("out-one")), "stdout streamed");
        assert!(shown.iter().any(|l| l.contains("err-two")), "stderr streamed");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_command_kills_on_silence_timeout() {
        let start = std::time::Instant::now();
        let r = run_command_with_limits(
            Path::new("cmd.exe"),
            &["/C".to_string(), "ping -n 6 127.0.0.1 >nul".to_string()],
            &|_: deploy::StageEvent| {},
            Duration::from_secs(2),
            Duration::from_secs(60),
        )
        .await;
        assert!(r.is_err(), "silence must kill the process");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "killed fast, not after total timeout"
        );
    }

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-tlauncher-{tag}"))
    }
}
