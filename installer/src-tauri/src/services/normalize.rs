//! Нормализация zip-карты во внутреннюю модель `Pack` (§14 ТЗ, §4 дизайн-дока).
//!
//! Конвейер главного пути: `zip → модель → деплой`, без промежуточного
//! `.mrpack`-файла. Анализ только читает центральный каталог архива
//! (без распаковки на диск), находит `level.dat`, классифицирует слои.

use crate::domain::{InstallError, LoaderKind, ModInfo, PackAnalysis, ResourcePackInfo, ResourcePackKind};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Жёсткие лимиты безопасности (ZipSlip-чеклист + антибомбы, D10).
const MAX_ENTRIES: usize = 100_000;
const MAX_TOTAL_UNCOMPRESSED: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SINGLE_FILE: u64 = 2 * 1024 * 1024 * 1024;

/// Слои пака, которые деплоятся из корня архива.
const KNOWN_LAYERS: &[&str] = &["mods", "config", "resourcepacks", "shaderpacks"];

/// Файлы настроек из корня игры (стандарт Minecraft): деплоятся в корень
/// игры как есть. Список закрытый и консервативный: всё остальное в корне —
/// в «Подробнее», а не молча. `launcher_profiles.json` и подобное с
/// аккаунтами — НИКОГДА (это данные лаунчера, не карты).
pub(crate) const ROOT_GAME_FILES: &[&str] = &[
    "options.txt",
    "optionsof.txt",
    "servers.dat",
    "usernamecache.json",
    "usercache.json",
];

/// Есть ли alias `overrides/` (в корне контента лежит modrinth.index.json —
/// автор упаковал распакованный модпак).
pub(crate) fn has_overrides_alias(names: &[String], content_prefix: &str) -> bool {
    names
        .iter()
        .any(|n| n == &format!("{content_prefix}modrinth.index.json"))
}

/// Путь записи относительно корня контента со срезанным alias `overrides/`.
/// `None`: вне корня, сама обёртка/overrides, манифест пака.
/// Единое правило для анализа и деплоя.
pub(crate) fn content_rel(
    raw: &str,
    content_prefix: &str,
    overrides_alias: bool,
) -> Option<String> {
    let rel = raw.strip_prefix(content_prefix)?;
    if rel.is_empty() {
        return None; // сама папка-обёртка
    }
    if overrides_alias {
        if rel == "modrinth.index.json" {
            return None; // манифест пака — не контент
        }
        if let Some(stripped) = rel.strip_prefix("overrides/") {
            return if stripped.is_empty() {
                None // сама папка overrides/
            } else {
                Some(stripped.to_string())
            };
        }
    }
    Some(rel.to_string())
}

/// Корневой файл настроек? Без учёта регистра (`Options.txt`), только в
/// корне контента (без `/`).
pub(crate) fn is_root_game_file(rel: &str) -> bool {
    if rel.contains('/') {
        return false;
    }
    let lower = rel.to_ascii_lowercase();
    ROOT_GAME_FILES.contains(&lower.as_str())
}

/// Декодирует сырое имя записи zip: строгий UTF-8 → CP866 (старые русские
/// архивы без UTF-8 флага, напр. мир «Охотник…» в DH.zip) → lossy.
/// Никогда не паникует; ZipSlip-проверка идёт отдельно по `enclosed_name`.
pub(crate) fn decode_name(raw: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for &b in raw {
        out.push(cp866_to_char(b));
    }
    // Если и CP866 дал мусор (вопросительные бокс-символы повально) —
    // это всё равно лучше паники: запись классифицируется и ставится.
    out
}

/// Один байт CP866 в char. ASCII — как есть, 0x80–0xFF — по таблице,
/// box-drawing и символы сохраняем (имена с ними не фильтруем).
fn cp866_to_char(b: u8) -> char {
    // Кириллица: верхний регистр 0x80–0x9F, нижний а–п 0xA0–0xAF,
    // нижний р–я 0xE0–0xEF, Ё/ё 0xF0/0xF1 (+ укр./бел. F2–F7).
    const UPPER: &str = "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ";
    const LOWER_AP: &str = "абвгдежзийклмноп";
    const LOWER_RYA: &str = "рстуфхцчшщъыьэюя";
    match b {
        0x00..=0x7F => b as char,
        0x80..=0x9F => UPPER.chars().nth((b - 0x80) as usize).unwrap_or('�'),
        0xA0..=0xAF => LOWER_AP.chars().nth((b - 0xA0) as usize).unwrap_or('�'),
        0xE0..=0xEF => LOWER_RYA.chars().nth((b - 0xE0) as usize).unwrap_or('�'),
        0xF0 => 'Ё',
        0xF1 => 'ё',
        0xF2 => 'Є',
        0xF3 => 'є',
        0xF4 => 'Ї',
        0xF5 => 'ї',
        0xF6 => 'Ў',
        0xF7 => 'ў',
        // B–D ряды и F8–FF: рамки и символы — сохраняем ближайшим юникодом.
        0xB0 => '░',
        0xB1 => '▒',
        0xB2 => '▓',
        0xB3 => '│',
        0xB4 => '┤',
        0xB5 => '╡',
        0xB6 => '╢',
        0xB7 => '╖',
        0xB8 => '╕',
        0xB9 => '╣',
        0xBA => '║',
        0xBB => '╗',
        0xBC => '╝',
        0xBD => '╜',
        0xBE => '╛',
        0xBF => '┐',
        0xC0 => '└',
        0xC1 => '┴',
        0xC2 => '┬',
        0xC3 => '├',
        0xC4 => '─',
        0xC5 => '┼',
        0xC6 => '╞',
        0xC7 => '╟',
        0xC8 => '╚',
        0xC9 => '╔',
        0xCA => '╩',
        0xCB => '╦',
        0xCC => '╠',
        0xCD => '═',
        0xCE => '╬',
        0xCF => '╧',
        0xD0 => '╨',
        0xD1 => '╤',
        0xD2 => '╥',
        0xD3 => '╙',
        0xD4 => '╘',
        0xD5 => '╒',
        0xD6 => '╓',
        0xD7 => '╫',
        0xD8 => '╪',
        0xD9 => '┌',
        0xDA => '█',
        0xDB => '▄',
        0xDC => '▌',
        0xDD => '▐',
        0xDE => '▀',
        0xDF => ' ',
        0xF8 => '°',
        0xF9 => '∙',
        0xFA => '·',
        0xFB => '√',
        0xFC => '№',
        0xFD => '¤',
        0xFE => '■',
        0xFF => ' ',
    }
}

/// Анализирует zip-архив карты и строит модель пака.
pub fn analyze_zip(path: &Path) -> Result<PackAnalysis, InstallError> {
    let file = std::fs::File::open(path).map_err(InstallError::ZipOpen)?;
    let file_size_bytes = file.metadata().map_err(InstallError::Io)?.len();
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "map.zip".to_string());
    let sha256 = sha256_of(path)?;

    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(InstallError::LimitExceeded(format!(
            "entries {} > {MAX_ENTRIES}",
            archive.len()
        )));
    }

    // Проход 1: инвентаризация имён, поиск level.dat, ZipSlip-проверка.
    // Имена декодируем сами (UTF-8 → CP866): name()/by_name() крейта
    // расходятся на не-UTF8 архивах. Доступ дальше — только по индексам.
    let mut total_uncompressed: u64 = 0;
    let mut world_roots: BTreeSet<String> = BTreeSet::new();
    let mut names: Vec<String> = Vec::with_capacity(archive.len());
    let mut level_dat_index: Option<usize> = None;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        let raw = decode_name(entry.name_raw());
        // ZipSlip: доверяем только путям, которые санитизирует сам zip-крейт.
        if entry.enclosed_name().is_none() {
            return Err(InstallError::ZipSlip(raw));
        }
        total_uncompressed = total_uncompressed.saturating_add(entry.size());
        if entry.size() > MAX_SINGLE_FILE {
            return Err(InstallError::LimitExceeded(format!(
                "entry {raw} exceeds single-file limit"
            )));
        }
        if raw.ends_with("level.dat") && !entry.is_dir() {
            let parent = parent_prefix(&raw);
            world_roots.insert(parent);
            level_dat_index = Some(i);
        }
        names.push(raw);
    }
    if total_uncompressed > MAX_TOTAL_UNCOMPRESSED {
        return Err(InstallError::LimitExceeded(format!(
            "uncompressed {total_uncompressed} bytes exceeds limit"
        )));
    }

    if world_roots.is_empty() {
        return Err(InstallError::NoLevelDat);
    }
    if world_roots.len() > 1 {
        let mut roots: Vec<String> = world_roots.into_iter().collect();
        roots.sort();
        return Err(InstallError::MultipleWorlds(roots));
    }
    let world_prefix = world_roots.into_iter().next().expect("checked non-empty");
    let world_name = world_name_of(&world_prefix);

    // Правило deploy-all без исключений: в инстанс едет ВСЁ из архива.
    // Поддерево мира ставит deploy_world целиком, всё остальное —
    // deploy_mods (всё под mods/) и deploy_layers (оверлей с сохранением
    // структуры). Никаких отсечений «мусора» и «неизвестного»: сложные карты
    // возят самописные файлы в произвольных местах (mods/.index/*.toml,
    // mods/pm/images/*.png, УСЛОВИЯ*.txt), и только автор знает, что важно.
    // Папка-обёртка (MyMap/ при мире MyMap/saves/Мир/) сама не переезжает —
    // только её содержимое. Файлы вне корня контента (напр. readme у корня
    // при обёртке) тоже ставятся — по полному пути.
    // Владение папками при деплое: мир — deploy_world,
    // mods/ — deploy_mods, всё остальное — deploy_layers оверлеем
    // с бэкапом заменяемых файлов.
    // Особый случай: автор упаковал распакованный модпак (в корне лежит
    // modrinth.index.json) — тогда ведущий "overrides/" это псевдоним
    // корня, а не папка для копирования.
    let content_prefix = detect_content_prefix(&names, &world_prefix);
    let overrides_alias = has_overrides_alias(&names, &content_prefix);
    let mut layers: BTreeSet<String> = BTreeSet::new();
    let mut mod_jars: Vec<(usize, String)> = Vec::new();
    for (idx, raw) in names.iter().enumerate() {
        if *raw == world_prefix || raw.starts_with(&world_prefix) {
            continue; // содержимое мира ставится как есть
        }
        // Путь относительно корня контента; записи вне его тоже ставятся
        // (по полному пути) — см. deploy_layers.
        let Some(rel) = content_rel(raw, &content_prefix, overrides_alias) else {
            continue;
        };
        if is_root_game_file(&rel) {
            // Настройки корня игры (options.txt и др.) — отдельный слой.
            layers.insert("root".to_string());
            continue;
        }
        let top = top_segment(&rel);
        if top == "datapacks" {
            // Корневые datapacks относятся к миру (деплой внутрь мира).
            layers.insert("datapacks".to_string());
        } else if KNOWN_LAYERS.contains(&top.as_str()) {
            layers.insert(top.clone());
            if top == "mods" {
                let under_mods = rel.strip_prefix("mods/").unwrap_or("");
                if rel.ends_with(".jar") && !under_mods.contains('/') {
                    mod_jars.push((idx, raw.clone()));
                }
                // Все остальные файлы под mods/ (вложенные jar, .toml,
                // .png, подпапки) тоже ставятся — см. deploy_mods.
            }
        }
        // Всё остальное (папки и файлы кастомного контента на любой глубине)
        // ставится оверлеем — отдельный учёт не нужен.
    }

    // Версия мира из level.dat (доступ по индексу — см. выше).
    let world_version = read_world_version(
        &mut archive,
        level_dat_index.expect("single world checked above"),
    )?;

    // Loose-ресурспаки рядом с миром (папки и zip вне мира и вне
    // `resourcepacks/`): маркер `pack.mcmeta`, как `level.dat` у миров.
    let resource_packs =
        detect_resource_packs(&mut archive, &names, &world_prefix, &content_prefix, overrides_alias)?;

    // Моды: читаем манифесты внутри jar (индекс + имя для отчётности).
    let mut mods: BTreeMap<String, ModInfo> = BTreeMap::new();
    let mut loader_votes: BTreeMap<LoaderKind, usize> = BTreeMap::new();
    let mut loader_hint: Option<String> = None;
    for (jar_index, jar) in &mod_jars {
        let file_name = jar.rsplit('/').next().unwrap_or(jar).to_string();
        match read_mod_info(&mut archive, *jar_index)? {
            Some(info) => {
                *loader_votes.entry(info.loader).or_insert(0) += 1;
                if loader_hint.is_none() {
                    loader_hint = info.version_hint.clone();
                }
                let key = jar.clone();
                let value = info.into_mod(file_name, mod_size(&mut archive, *jar_index));
                mods.insert(key, value);
            }
            None => {
                // Jar без опознавательных манифестов (напр. OptiFine):
                // встраиваем как есть, загрузчик уточним после.
                let stem = file_name
                    .strip_suffix(".jar")
                    .unwrap_or(&file_name)
                    .to_string();
                mods.insert(
                    jar.clone(),
                    ModInfo {
                        file_name,
                        mod_id: stem,
                        version: None,
                        loader: LoaderKind::Vanilla, // заполнитель, см. ниже
                        size_bytes: mod_size(&mut archive, *jar_index),
                    },
                );
            }
        }
    }

    let is_modded = !mod_jars.is_empty();
    let loader = if !is_modded {
        LoaderKind::Vanilla
    } else {
        // Верим содержимому: побеждает загрузчик большинства опознанных jar.
        // Ни один не опознан → явная ошибка, не гадаем (D10).
        match loader_votes.into_iter().max_by_key(|(_, n)| *n) {
            Some((loader, _)) => loader,
            None => {
                return Err(InstallError::UnknownLoader(
                    "в mods/ есть jar, но ни в одном нет fabric.mod.json, mods.toml или quilt.json — укажите загрузчик вручную".to_string(),
                ))
            }
        }
    };
    for info in mods.values_mut() {
        if info.loader == LoaderKind::Vanilla && is_modded {
            info.loader = loader; // неопознанные jar наследуют загрузчик пака
        }
    }

    let mut layers: Vec<String> = layers.into_iter().collect();
    layers.sort();

    Ok(PackAnalysis {
        file_name,
        file_size_bytes,
        sha256,
        world_name,
        world_version,
        is_modded,
        loader,
        loader_version_hint: loader_hint,
        mods: mods.into_values().collect(),
        layers,
        total_uncompressed_bytes: total_uncompressed,
        entry_count: names.len(),
        world_prefix,
        content_prefix,
        overrides_alias,
        resource_packs,
    })
}

/// Корень контента: самый глубокий предок мира (включая корень архива),
/// под которым есть хоть что-то кроме самого мира (слои, кастомные файлы,
/// настройки). Плоский архив → `""`.
/// Пример: мир `MyMap/saves/Мир/`, рядом `MyMap/mods/` → `"MyMap/"`.
/// Сама `MyMap/` при этом не переезжает — переезжает её содержимое.
fn detect_content_prefix(names: &[String], world_prefix: &str) -> String {
    // Предки от глубокого к корню: "A/B/" → ["A/", ""].
    let mut ancestors: Vec<String> = Vec::new();
    let mut rest = world_prefix.trim_end_matches('/').to_string();
    while let Some(i) = rest.rfind('/') {
        rest.truncate(i); // без слэша, иначе rfind найдёт его же вечно
        ancestors.push(format!("{rest}/"));
    }
    ancestors.push(String::new());

    for candidate in ancestors {
        let has_content = names.iter().any(|raw| {
            if *raw == world_prefix || raw.starts_with(world_prefix) {
                return false; // сам мир — не контент
            }
            let rel = match raw.strip_prefix(&candidate) {
                Some(r) => r,
                None => return false,
            };
            // Пустой rel — сам кандидат (папка-обёртка), не контент.
            !rel.is_empty()
        });
        if has_content {
            return candidate;
        }
    }
    String::new()
}

/// Loose-ресурспаки рядом с миром: папки (`<root>pack.mcmeta` + `<root>assets/…`)
/// и zip-файлы с `pack.mcmeta` + `assets/` в корне внутреннего архива.
/// Поиск — как у миров через `level.dat`, только маркер другой.
///
/// Исключения (не loose, ставить отдельно не надо):
/// - всё под `world_prefix` — встроенные (`<мир>/resources.zip`,
///   `<мир>/datapacks/*/pack.mcmeta`): едут вместе с миром, игра подхватывает;
/// - всё уже под `resourcepacks/` — ставит оверлей как есть;
/// - корень, содержащий мир (`world_prefix` начинается с него, напр. сам
///   `content_prefix`) — это контейнер карты, а не пак;
/// - кандидаты без `assets/` — датапаки (у них `data/`), не наша забота.
///
/// Имена сравниваем строго в нижнем регистре Minecraft (`pack.mcmeta`,
/// `assets`): с 1.11 игра требует строчные имена внутри паков.
fn detect_resource_packs(
    archive: &mut zip::ZipArchive<std::fs::File>,
    names: &[String],
    world_prefix: &str,
    content_prefix: &str,
    overrides_alias: bool,
) -> Result<Vec<ResourcePackInfo>, InstallError> {
    let mut packs: Vec<ResourcePackInfo> = Vec::new();

    // Папки: родитель каждого `pack.mcmeta`.
    for (idx, raw) in names.iter().enumerate() {
        if !raw.ends_with("pack.mcmeta") || raw.ends_with('/') {
            continue;
        }
        let root = parent_prefix(raw);
        if root.is_empty() {
            continue; // pack.mcmeta в корне архива — не loose-пак
        }
        if raw == world_prefix || raw.starts_with(world_prefix) {
            continue; // внутри мира — встроенный, едет с миром
        }
        if is_in_resourcepacks_tree(raw, content_prefix, overrides_alias) {
            continue; // уже на месте — ставит оверлей
        }
        if world_prefix.starts_with(&root) {
            continue; // корень содержит мир — контейнер карты
        }
        let assets_prefix = format!("{root}assets/");
        if !names
            .iter()
            .any(|n| *n == assets_prefix || n.starts_with(&assets_prefix))
        {
            continue; // нет assets/ — датапак или мусор
        }
        let name = root
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(&root)
            .to_string();
        let description = read_entry_bytes(archive, idx)
            .ok()
            .and_then(|b| pack_description(&b));
        packs.push(ResourcePackInfo {
            name,
            source: root,
            kind: ResourcePackKind::Folder,
            description,
        });
    }

    // Zip: файлы с `pack.mcmeta` + `assets/` в корне внутреннего архива.
    let folder_roots: Vec<String> = packs.iter().map(|p| p.source.clone()).collect();
    for (idx, raw) in names.iter().enumerate() {
        if raw.ends_with('/') || !raw.to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        if raw.starts_with(world_prefix) {
            continue; // внутри мира — встроенный resources.zip
        }
        if is_in_resourcepacks_tree(raw, content_prefix, overrides_alias) {
            continue; // уже на месте — ставит оверлей файлом
        }
        if folder_roots.iter().any(|r| raw != r && raw.starts_with(r)) {
            continue; // zip внутри папки-пака — часть пака
        }
        let Some(inner) = read_inner_zip(archive, idx)? else {
            continue; // не читается как zip или не пак — не наше
        };
        if !inner.has_pack_mcmeta || !inner.has_assets {
            continue;
        }
        let name = raw.rsplit('/').next().unwrap_or(raw).to_string();
        packs.push(ResourcePackInfo {
            name,
            source: raw.clone(),
            kind: ResourcePackKind::Zip,
            description: inner.description,
        });
    }

    packs.sort_by(|a, b| a.name.cmp(&b.name).then(a.source.cmp(&b.source)));
    Ok(packs)
}

/// Запись уже лежит в дереве `resourcepacks/` (от корня контента с учётом
/// alias `overrides/` либо рядом с обёрткой у корня архива): её ставит
/// оверлей, отдельным паком не считаем.
fn is_in_resourcepacks_tree(raw: &str, content_prefix: &str, overrides_alias: bool) -> bool {
    if let Some(rel) = content_rel(raw, content_prefix, overrides_alias) {
        return rel == "resourcepacks" || rel.starts_with("resourcepacks/");
    }
    raw == "resourcepacks" || raw.starts_with("resourcepacks/")
}

/// Сводка по корню внутреннего zip: есть ли маркеры ресурспака.
struct InnerPackProbe {
    has_pack_mcmeta: bool,
    has_assets: bool,
    description: Option<String>,
}

/// Читает запись-zip во временный файл и проверяет корень внутреннего архива.
/// `Ok(None)` — запись не zip или внутри нет маркеров пака (не ошибка:
/// это обычный вложенный архив). Временный файл удаляется всегда.
fn read_inner_zip(
    archive: &mut zip::ZipArchive<std::fs::File>,
    index: usize,
) -> Result<Option<InnerPackProbe>, InstallError> {
    let bytes = read_entry_bytes(archive, index)?;
    let dir = std::env::temp_dir().join("kraimaps");
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(
        "rp_probe_{}_{}.zip",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, &bytes)?;
    let result = (|| -> Option<InnerPackProbe> {
        let file = std::fs::File::open(&tmp).ok()?;
        let mut inner = zip::ZipArchive::new(file).ok()?;
        let mut probe = InnerPackProbe {
            has_pack_mcmeta: false,
            has_assets: false,
            description: None,
        };
        for i in 0..inner.len() {
            let mut entry = inner.by_index(i).ok()?;
            let name = entry.name().to_string();
            if name == "pack.mcmeta" && !entry.is_dir() {
                probe.has_pack_mcmeta = true;
                let mut buf = Vec::new();
                if entry.read_to_end(&mut buf).is_ok() {
                    probe.description = pack_description(&buf);
                }
            } else if name == "assets/" || name.starts_with("assets/") {
                probe.has_assets = true;
            }
            if probe.has_pack_mcmeta && probe.has_assets && probe.description.is_some() {
                break;
            }
        }
        Some(probe)
    })();
    std::fs::remove_file(&tmp).ok();
    Ok(result)
}

/// Байты записи архива по индексу (имена бывают не-UTF8 — только индексы).
fn read_entry_bytes(
    archive: &mut zip::ZipArchive<std::fs::File>,
    index: usize,
) -> Result<Vec<u8>, InstallError> {
    let mut entry = archive
        .by_index(index)
        .map_err(|e| InstallError::ZipCorrupt(format!("entry unreadable: {e}")))?;
    let mut buf = Vec::with_capacity(entry.size().min(1 << 20) as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(|e| InstallError::ZipCorrupt(format!("entry unreadable: {e}")))?;
    Ok(buf)
}

/// `pack.description` из `pack.mcmeta` для подписи в UI (best-effort).
/// Понимает и старый `pack_format`, и новый `min_format`/`max_format` —
/// важен только текст: отсутствие/невалидность JSON — это `None`, не ошибка.
/// Нетекстовые описания (translate-компоненты) в UI не тащим.
fn pack_description(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let desc = v.get("pack")?.get("description")?;
    let s = desc.as_str()?;
    let clean: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = clean.chars().take(120).collect();
    if short.trim().is_empty() {
        None
    } else {
        Some(short)
    }
}

/// Родитель `level.dat` с `/` на конце (`"DeepMine/"`, `"saves/Мир/"`, `""`).
fn parent_prefix(entry: &str) -> String {
    match entry.rfind('/') {
        Some(i) => entry[..=i].to_string(),
        None => String::new(),
    }
}

/// Имя мира = последний сегмент префикса.
fn world_name_of(prefix: &str) -> String {
    let trimmed = prefix.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Первый сегмент пути (`"mods/a.jar"` → `"mods"`).
fn top_segment(path: &str) -> String {
    match path.find('/') {
        Some(i) => path[..i].to_string(),
        None => path.to_string(),
    }
}

/// SHA-256 файла стримингом (карты по 100+ МБ целиком в память не грузим).
fn sha256_of(path: &Path) -> Result<String, InstallError> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path).map_err(InstallError::ZipOpen)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 1024 * 64];
    loop {
        let n = file.read(&mut buf).map_err(InstallError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_of(hasher.finalize()))
}

fn hex_of(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Читает `level.dat` из архива по индексу и извлекает `Version.Name`.
fn read_world_version(
    archive: &mut zip::ZipArchive<std::fs::File>,
    level_dat_index: usize,
) -> Result<Option<String>, InstallError> {
    let mut entry = archive
        .by_index(level_dat_index)
        .map_err(|e| InstallError::ZipCorrupt(format!("level.dat unreadable: {e}")))?;
    let mut raw = Vec::new();
    entry
        .read_to_end(&mut raw)
        .map_err(|e| InstallError::ZipCorrupt(format!("level.dat unreadable: {e}")))?;
    drop(entry);
    Ok(extract_world_version(&raw))
}

/// Извлекает `Version.Name` из сырого `level.dat` (gzipped NBT).
/// Возвращает `None`, если поле отсутствует или формат не разобран, —
/// отсутствие версии не должно ронять весь анализ.
pub(crate) fn extract_world_version(raw: &[u8]) -> Option<String> {
    let mut gz = flate2::read::GzDecoder::new(raw);
    let mut nbt = Vec::new();
    gz.read_to_end(&mut nbt).ok()?;
    let mut cursor = NbtCursor { data: &nbt, pos: 0 };
    // Корень: TAG_Compound с пустым именем. Путь к версии —
    // `Data → Version → Name` (ванилльный формат); запасной — `Version → Name`.
    if cursor.read_u8().ok()? != 10 {
        return None;
    }
    let name_len = cursor.read_u16().ok()? as usize;
    cursor.skip(name_len).ok()?;
    find_in_compound(&mut cursor, &["Data", "Version", "Name"])
        .or_else(|| {
            let mut retry = NbtCursor { data: &nbt, pos: 3 + name_len };
            find_in_compound(&mut retry, &["Version", "Name"])
        })
}

/// Поиск значения `path[0] -> ... -> path[n]` (строка) внутри компаунда.
fn find_in_compound(cursor: &mut NbtCursor, path: &[&str]) -> Option<String> {
    loop {
        let tag = cursor.read_u8().ok()?;
        if tag == 0 {
            return None; // TAG_End
        }
        let name = cursor.read_string().ok()?;
        if name == path[0] {
            if path.len() == 1 {
                return if tag == 8 {
                    cursor.read_string().ok()
                } else {
                    None
                };
            }
            // Спускаемся только в компаунд; иначе пропускаем значение.
            if tag != 10 {
                cursor.skip_payload(tag).ok()?;
                continue;
            }
            return find_in_compound(cursor, &path[1..]);
        }
        cursor.skip_payload(tag).ok()?;
    }
}

struct NbtCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> NbtCursor<'a> {
    fn read_u8(&mut self) -> Result<u8, ()> {
        let b = *self.data.get(self.pos).ok_or(())?;
        self.pos += 1;
        Ok(b)
    }
    fn read_u16(&mut self) -> Result<u16, ()> {
        if self.pos + 2 > self.data.len() {
            return Err(());
        }
        let v = u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }
    fn read_u32(&mut self) -> Result<u32, ()> {
        if self.pos + 4 > self.data.len() {
            return Err(());
        }
        let v = u32::from_be_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }
    fn skip(&mut self, n: usize) -> Result<(), ()> {
        if self.pos + n > self.data.len() {
            return Err(());
        }
        self.pos += n;
        Ok(())
    }
    fn read_string(&mut self) -> Result<String, ()> {
        let len = self.read_u16()? as usize;
        if self.pos + len > self.data.len() {
            return Err(());
        }
        let s = std::str::from_utf8(&self.data[self.pos..self.pos + len]).map_err(|_| ())?;
        self.pos += len;
        Ok(s.to_string())
    }
    /// Пропуск payload тега любого типа (для тегов, которые нам не нужны).
    fn skip_payload(&mut self, tag: u8) -> Result<(), ()> {
        match tag {
            1 => self.skip(1),
            2 => self.skip(2),
            3 => self.skip(4),
            4 => self.skip(8),
            5 => self.skip(4),
            6 => self.skip(8),
            7 => {
                let len = self.read_u32()? as usize;
                self.skip(len)
            }
            8 => {
                let len = self.read_u16()? as usize;
                self.skip(len)
            }
            9 => {
                let elem = self.read_u8()?;
                let len = self.read_u32()? as usize;
                if elem == 0 || len == 0 {
                    return Ok(());
                }
                // Списки компаундов/массивов рекурсивно не разбираем:
                // для поиска Version.Name они не нужны, но пропустить надо честно.
                for _ in 0..len {
                    self.skip_payload_no_list(elem)?;
                }
                Ok(())
            }
            10 => loop {
                let inner = self.read_u8()?;
                if inner == 0 {
                    return Ok(());
                }
                let nlen = self.read_u16()? as usize;
                self.skip(nlen)?;
                self.skip_payload(inner)?;
            },
            11 => {
                let len = self.read_u32()? as usize;
                self.skip(len.saturating_mul(4))
            }
            12 => {
                let len = self.read_u32()? as usize;
                self.skip(len.saturating_mul(8))
            }
            _ => Err(()),
        }
    }
    /// Пропуск элемента списка (вложенные списки запрещены форматом NBT).
    fn skip_payload_no_list(&mut self, tag: u8) -> Result<(), ()> {
        if tag == 9 {
            return Err(());
        }
        // У элементов списка нет имён — только payload.
        match tag {
            10 => loop {
                let inner = self.read_u8()?;
                if inner == 0 {
                    return Ok(());
                }
                let nlen = self.read_u16()? as usize;
                self.skip(nlen)?;
                self.skip_payload(inner)?;
            },
            _ => self.skip_payload(tag),
        }
    }
}

/// Сырые сведения из манифеста мода внутри jar.
struct RawModMeta {
    loader: LoaderKind,
    mod_id: String,
    version: Option<String>,
    /// Подсказка версии загрузчика (forge `loaderVersion` вида `[40,)`).
    version_hint: Option<String>,
}

impl RawModMeta {
    fn into_mod(self, file_name: String, size_bytes: u64) -> ModInfo {
        ModInfo {
            file_name,
            mod_id: self.mod_id,
            version: self.version,
            loader: self.loader,
            size_bytes,
        }
    }
}

/// Размер записи в архиве (для бейджей и прогресса). По индексу:
/// имена бывают не-UTF8, `by_name` с ними расходится.
fn mod_size(archive: &mut zip::ZipArchive<std::fs::File>, index: usize) -> u64 {
    archive.by_index(index).map(|e| e.size()).unwrap_or(0)
}

/// Читает манифесты мода внутри `mods/*.jar` (доступ по индексу — см. выше).
/// Возвращает `None`, если jar неопознан (встраиваем как есть, напр. OptiFine).
fn read_mod_info(
    archive: &mut zip::ZipArchive<std::fs::File>,
    jar_index: usize,
) -> Result<Option<RawModMeta>, InstallError> {
    let mut jar_bytes = Vec::new();
    archive
        .by_index(jar_index)
        .map_err(|e| InstallError::ZipCorrupt(format!("mod unreadable: {e}")))?
        .read_to_end(&mut jar_bytes)
        .map_err(|e| InstallError::ZipCorrupt(format!("mod unreadable: {e}")))?;
    let mut jar = zip::ZipArchive::new(std::io::Cursor::new(jar_bytes))
        .map_err(|e| InstallError::ZipCorrupt(format!("mod is not a jar: {e}")))?;

    // NeoForge определяется отдельным файлом — проверяем первым.
    if jar.by_name("META-INF/neoforge.mods.toml").is_ok() {
        let meta = parse_mods_toml(&mut jar, "META-INF/neoforge.mods.toml")?;
        return Ok(meta.map(|m| RawModMeta {
            loader: LoaderKind::NeoForge,
            ..m
        }));
    }
    if jar.by_name("META-INF/mods.toml").is_ok() {
        let meta = parse_mods_toml(&mut jar, "META-INF/mods.toml")?;
        return Ok(meta.map(|m| RawModMeta {
            loader: LoaderKind::Forge,
            ..m
        }));
    }
    if jar.by_name("fabric.mod.json").is_ok() {
        return Ok(parse_fabric_mod_json(&mut jar)?);
    }
    if jar.by_name("quilt.json").is_ok() {
        return Ok(parse_quilt_json(&mut jar)?);
    }
    Ok(None)
}

/// Парсит `mods.toml` (Forge/NeoForge): `modId`, `version`, `loaderVersion`.
fn parse_mods_toml(
    jar: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
    path: &str,
) -> Result<Option<RawModMeta>, InstallError> {
    let mut text = String::new();
    jar.by_name(path)
        .map_err(|e| InstallError::ZipCorrupt(format!("mods.toml unreadable: {e}")))?
        .read_to_string(&mut text)
        .map_err(|e| InstallError::ZipCorrupt(format!("mods.toml unreadable: {e}")))?;
    let doc: toml::Value = text
        .parse()
        .map_err(|e| InstallError::ZipCorrupt(format!("mods.toml invalid: {e}")))?;

    let mod_id = doc
        .get("mods")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("modId"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let version = doc
        .get("mods")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // `loaderVersion`: сначала корень документа (как в настоящих mods.toml),
    // затем первая секция dependencies с этим полем.
    let mut version_hint = doc
        .get("loaderVersion")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if version_hint.is_none() {
        if let Some(deps) = doc.get("dependencies").and_then(|v| v.as_table()) {
            'outer: for table in deps.values().filter_map(|v| v.as_array()) {
                for dep in table.iter().filter_map(|v| v.as_table()) {
                    if let Some(lv) = dep.get("loaderVersion").and_then(|v| v.as_str()) {
                        version_hint = Some(lv.to_string());
                        break 'outer;
                    }
                }
            }
        }
    }
    Ok(Some(RawModMeta {
        loader: LoaderKind::Forge, // перезаписывается вызывателем
        mod_id,
        version,
        version_hint,
    }))
}

/// Парсит `fabric.mod.json`: `id`, `version`.
fn parse_fabric_mod_json(
    jar: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
) -> Result<Option<RawModMeta>, InstallError> {
    let mut text = String::new();
    jar.by_name("fabric.mod.json")
        .map_err(|e| InstallError::ZipCorrupt(format!("fabric.mod.json unreadable: {e}")))?
        .read_to_string(&mut text)
        .map_err(|e| InstallError::ZipCorrupt(format!("fabric.mod.json unreadable: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| InstallError::ZipCorrupt(format!("fabric.mod.json invalid: {e}")))?;
    Ok(Some(RawModMeta {
        loader: LoaderKind::Fabric,
        mod_id: v
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        version: v
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        version_hint: None,
    }))
}

/// Парсит `quilt.json` (quilt_loader): `quilt_loader.id`, `version`.
fn parse_quilt_json(
    jar: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
) -> Result<Option<RawModMeta>, InstallError> {
    let mut text = String::new();
    jar.by_name("quilt.json")
        .map_err(|e| InstallError::ZipCorrupt(format!("quilt.json unreadable: {e}")))?
        .read_to_string(&mut text)
        .map_err(|e| InstallError::ZipCorrupt(format!("quilt.json unreadable: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| InstallError::ZipCorrupt(format!("quilt.json invalid: {e}")))?;
    let q = &v["quilt_loader"];
    Ok(Some(RawModMeta {
        loader: LoaderKind::Quilt,
        mod_id: q
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        version: q
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        version_hint: None,
    }))
}

/// Корень игры в инстансе Prism-семейства: `minecraft/`, затем legacy-варианты
/// (§6.7 ТЗ с поправкой спайка: корень — `minecraft/`, не `.minecraft/`).
/// Layout общий для Prism, Pinecone, PolyMC и MultiMC.
pub fn prism_game_root(instance_dir: &Path) -> PathBuf {
    for candidate in ["minecraft", ".minecraft", ""] {
        let dir = if candidate.is_empty() {
            instance_dir.to_path_buf()
        } else {
            instance_dir.join(candidate)
        };
        if dir.join("saves").is_dir() || candidate == "minecraft" {
            return dir;
        }
    }
    instance_dir.join("minecraft")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_zip_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mapinstaller-test-{}-{}-{}.zip",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        p
    }

    /// Минимальный `level.dat`: gzipped NBT `{Version: {Id, Name, Series}}`.
    fn level_dat_bytes(version_name: &str) -> Vec<u8> {
        fn tag_string(out: &mut Vec<u8>, name: &str, value: &str) {
            out.push(8);
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(value.len() as u16).to_be_bytes());
            out.extend_from_slice(value.as_bytes());
        }
        let mut nbt = vec![10, 0, 0]; // TAG_Compound ""
        nbt.push(10); // TAG_Compound "Data" (ванилльный layout)
        nbt.extend_from_slice(&4u16.to_be_bytes());
        nbt.extend_from_slice(b"Data");
        // Поля ДО Version — проверяют честный пропуск payload всех типов.
        nbt.push(4); // TAG_Long "LastPlayed" (8 байт, не 4!)
        nbt.extend_from_slice(&10u16.to_be_bytes());
        nbt.extend_from_slice(b"LastPlayed");
        nbt.extend_from_slice(&123i64.to_be_bytes());
        nbt.push(5); // TAG_Float "SpawnAngle" (4 байта, не 8!)
        nbt.extend_from_slice(&10u16.to_be_bytes());
        nbt.extend_from_slice(b"SpawnAngle");
        nbt.extend_from_slice(&0f32.to_be_bytes());
        nbt.push(9); // TAG_List строк "ServerBrands" — пропуск списка
        nbt.extend_from_slice(&12u16.to_be_bytes());
        nbt.extend_from_slice(b"ServerBrands");
        nbt.push(8);
        nbt.extend_from_slice(&2u32.to_be_bytes());
        for brand in ["vanilla", "test"] {
            nbt.extend_from_slice(&(brand.len() as u16).to_be_bytes());
            nbt.extend_from_slice(brand.as_bytes());
        }
        nbt.push(10); // TAG_Compound "Version"
        nbt.extend_from_slice(&7u16.to_be_bytes());
        nbt.extend_from_slice(b"Version");
        nbt.push(3); // TAG_Int "Id"
        nbt.extend_from_slice(&2u16.to_be_bytes());
        nbt.extend_from_slice(b"Id");
        nbt.extend_from_slice(&1i32.to_be_bytes());
        tag_string(&mut nbt, "Name", version_name);
        tag_string(&mut nbt, "Series", "main");
        nbt.push(0); // TAG_End Version
        nbt.push(0); // TAG_End Data
        nbt.push(0); // TAG_End root

        let mut enc = flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        );
        enc.write_all(&nbt).expect("gzip");
        enc.finish().expect("gzip finish")
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start_file");
            zip.write_all(data).expect("write entry");
        }
        zip.finish().expect("finish zip");
    }

    fn forge_jar_bytes() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let toml = r#"
modLoader="javafml"
loaderVersion="[40,)"
[[mods]]
modId="towerofdoom"
version="1.0.0"
[[dependencies.towerofdoom]]
modId="forge"
mandatory=true
versionRange="[40,)"
[[dependencies.towerofdoom]]
modId="minecraft"
mandatory=true
versionRange="[1.18.2]"
"#;
            zip.start_file(
                "META-INF/mods.toml",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("mods.toml");
            zip.write_all(toml.as_bytes()).expect("write toml");
            zip.finish().expect("finish jar");
        }
        buf.into_inner()
    }

    #[test]
    fn vanilla_zip_is_classified() {
        let path = temp_zip_path("vanilla");
        let level = level_dat_bytes("1.21.11");
        write_zip(
            &path,
            &[
                ("DeepMine/level.dat", &level),
                ("DeepMine/region/r.0.0.mca", b"fake-region"),
                ("DeepMine/session.lock", b"junk"),
                ("DeepMine/level.dat_old", b"junk"),
                ("__MACOSX/._DeepMine", b"junk"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.world_name, "DeepMine");
        assert_eq!(pack.world_version.as_deref(), Some("1.21.11"));
        assert!(!pack.is_modded);
        assert_eq!(pack.loader, LoaderKind::Vanilla);
        assert_eq!(pack.world_prefix, "DeepMine/");
        assert!(!pack.sha256.is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn saves_wrapped_layout_is_supported() {
        let path = temp_zip_path("saves");
        let level = level_dat_bytes("1.20.4");
        write_zip(&path, &[("saves/MyWorld/level.dat", &level)]);

        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.world_name, "MyWorld");
        assert_eq!(pack.world_prefix, "saves/MyWorld/");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn modded_zip_detects_forge_loader() {
        let path = temp_zip_path("modded");
        let level = level_dat_bytes("1.18.2");
        let jar = forge_jar_bytes();
        let plain = b"fake-optifine".to_vec();
        write_zip(
            &path,
            &[
                ("Tower of Doom/level.dat", &level),
                ("mods/towerofdoom.jar", &jar),
                ("mods/OptiFine.jar", &plain),
                ("Tower of Doom.docx", b"author notes"),
                ("serverconfig/server.toml", b"server junk"),
            ],
        );

        // plain jar is not a zip → corrupt? No: read_mod_info fails on
        // non-zip jar. OptiFine-like jars ARE zips; emulate with tiny zip.
        std::fs::remove_file(&path).ok();
        let mut optifine = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut optifine);
            zip.start_file(
                "OptiFine.class",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("class");
            zip.write_all(b"fake-class").expect("write");
            zip.finish().expect("finish");
        }
        let optifine = optifine.into_inner();
        write_zip(
            &path,
            &[
                ("Tower of Doom/level.dat", &level),
                ("mods/towerofdoom.jar", &jar),
                ("mods/OptiFine.jar", &optifine),
                ("Tower of Doom.docx", b"author notes"),
                ("serverconfig/server.toml", b"server junk"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(pack.is_modded);
        assert_eq!(pack.loader, LoaderKind::Forge);
        assert_eq!(pack.loader_version_hint.as_deref(), Some("[40,)"));
        assert_eq!(pack.mods.len(), 2);
        let tod = pack
            .mods
            .iter()
            .find(|m| m.mod_id == "towerofdoom")
            .expect("towerofdoom mod");
        assert_eq!(tod.version.as_deref(), Some("1.0.0"));
        // Кастомные верхние записи тоже ставятся (deploy-all без исключений).
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn decode_name_handles_cp866() {
        // «Охотник» в CP866 (живые байты из DH.zip, старые архивы без UTF-8).
        let cp866 = [0x8E, 0xE5, 0xAE, 0xE2, 0xAD, 0xA8, 0xAA];
        assert_eq!(decode_name(&cp866), "Охотник");
        // UTF-8 проходит как есть, включая кириллицу.
        assert_eq!(decode_name("Мир_1".as_bytes()), "Мир_1");
        assert_eq!(decode_name(b"mods/a.jar"), "mods/a.jar");
        // Мусор не роняет декодер.
        assert!(!decode_name(&[0xFF, 0xFE, 0x00]).is_empty());
    }

    #[test]
    fn cyrillic_world_deploys_end_to_end() {
        // Мир с кириллицей в UTF-8: имя сохраняется, деплой идёт по индексам.
        let path = temp_zip_path("cyrillic");
        let level = level_dat_bytes("1.20.1");
        write_zip(
            &path,
            &[("saves/Охотник/level.dat", &level), ("options.txt", b"gfx")],
        );
        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.world_name, "Охотник");
        assert_eq!(pack.world_version.as_deref(), Some("1.20.1"));
        assert!(pack.layers.contains(&"root".to_string()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn root_game_files_form_a_layer() {
        let path = temp_zip_path("rootfiles");
        let level = level_dat_bytes("1.20.1");
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("options.txt", b"gfx:fast"),
                ("servers.dat", b"servers"),
                ("mods/desktop.ini", b"windows junk"),
                ("notes.txt", b"stray"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(!pack.is_modded);
        assert!(pack.layers.contains(&"root".to_string()));
        // deploy-all без исключений: кастомный notes.txt и desktop.ini
        // тоже ставятся, отдельных списков пропущенного нет.
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrapper_folder_layout_is_unwrapped() {
        // Tower of Doom2: мир и моды под общей папкой-обёрткой.
        let path = temp_zip_path("wrapper");
        let level = level_dat_bytes("1.18.2");
        let jar = forge_jar_bytes();
        write_zip(
            &path,
            &[
                ("Pack/World/level.dat", &level),
                ("Pack/mods/towerofdoom.jar", &jar),
                ("Pack/config/opts.txt", b"cfg"),
                ("readme.txt", b"stray at root"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.world_name, "World");
        assert_eq!(pack.world_prefix, "Pack/World/");
        assert_eq!(pack.content_prefix, "Pack/");
        assert!(pack.is_modded);
        assert_eq!(pack.loader, LoaderKind::Forge);
        assert_eq!(pack.mods.len(), 1);
        assert!(pack.layers.contains(&"mods".to_string()));
        assert!(pack.layers.contains(&"config".to_string()));
        // Файлы вне корня контента (readme у корня при обёртке) тоже
        // ставятся — по полному пути, см. deploy_layers.
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn nested_mods_files_do_not_break_analysis() {
        // Сложные карты возят под mods/ самописные файлы: трекинг
        // (.index/*.toml), ресурсы кастомных модов (pm/images/*.png).
        // Они не участвуют в определении загрузчика, но и не отсекаются.
        let path = temp_zip_path("nestedmods");
        let level = level_dat_bytes("1.20.1");
        let jar = forge_jar_bytes();
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("mods/core.jar", &jar),
                ("mods/.index/core.pw.toml", b"meta"),
                ("mods/pm/images/1.png", b"png"),
                (
                    "УСЛОВИЯ ИСПОЛЬЗОВАНИЯ.txt",
                    "условия".as_bytes(),
                ),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(pack.is_modded);
        assert_eq!(pack.loader, LoaderKind::Forge);
        assert_eq!(pack.mods.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_level_dat_is_an_error() {
        let path = temp_zip_path("nolevel");
        write_zip(&path, &[("mods/a.jar", b"fake")]);
        let err = analyze_zip(&path).expect_err("must fail");
        assert_eq!(err.code(), "E_NO_LEVEL_DAT");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn two_different_worlds_are_an_error() {
        let path = temp_zip_path("twoworlds");
        let level = level_dat_bytes("1.20.1");
        write_zip(
            &path,
            &[
                ("WorldA/level.dat", &level),
                ("WorldB/level.dat", &level),
            ],
        );
        let err = analyze_zip(&path).expect_err("must fail");
        assert_eq!(err.code(), "E_MULTI_WORLD");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn zipslip_entry_is_rejected() {
        let path = temp_zip_path("slip");
        let level = level_dat_bytes("1.20.1");
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("../evil.txt", b"evil"),
            ],
        );
        let err = analyze_zip(&path).expect_err("must fail");
        assert_eq!(err.code(), "E_ZIPSLIP");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unknown_loader_without_manifests_is_an_error() {
        let path = temp_zip_path("nometa");
        let level = level_dat_bytes("1.18.2");
        let mut jar = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut jar);
            zip.start_file("NoMeta.class", zip::write::SimpleFileOptions::default())
                .expect("class");
            zip.write_all(b"fake").expect("write");
            zip.finish().expect("finish");
        }
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("mods/mystery.jar", &jar.into_inner()),
            ],
        );
        let err = analyze_zip(&path).expect_err("must fail");
        assert_eq!(err.code(), "E_UNKNOWN_LOADER");
        std::fs::remove_file(&path).ok();
    }

    /// Внутренний zip для тестов: пак (`pack.mcmeta` + `assets/`) или мусор.
    fn inner_pack_bytes(with_mcmeta: bool) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            if with_mcmeta {
                zip.start_file("pack.mcmeta", zip::write::SimpleFileOptions::default())
                    .expect("mcmeta");
                zip.write_all(
                    br#"{"pack":{"pack_format":15,"description":"Cute pack"}}"#,
                )
                .expect("write");
            } else {
                zip.start_file("notes.txt", zip::write::SimpleFileOptions::default())
                    .expect("notes");
                zip.write_all(b"just notes").expect("write");
            }
            zip.start_file(
                "assets/minecraft/x.txt",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("asset");
            zip.write_all(b"x").expect("write");
            zip.finish().expect("finish");
        }
        buf.into_inner()
    }

    #[test]
    fn loose_folder_pack_is_detected_with_new_format() {
        // MOTD2-кейс: папка пака рядом с миром, pack.mcmeta нового формата
        // (min_format/max_format без pack_format) — маркера достаточно.
        let path = temp_zip_path("looserp");
        let level = level_dat_bytes("1.21.11");
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                (
                    "MOTD2 ResourcePack/pack.mcmeta",
                    b"{\"pack\":{\"description\":\"Made By Test\",\"min_format\":75,\"max_format\":75}}"
                        .as_slice(),
                ),
                ("MOTD2 ResourcePack/assets/minecraft/sounds.json", b"{}"),
                ("MOTD2 ResourcePack/pack.png", b"png"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.resource_packs.len(), 1);
        let rp = &pack.resource_packs[0];
        assert_eq!(rp.name, "MOTD2 ResourcePack");
        assert_eq!(rp.source, "MOTD2 ResourcePack/");
        assert_eq!(rp.kind, ResourcePackKind::Folder);
        assert_eq!(rp.description.as_deref(), Some("Made By Test"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn world_embedded_packs_are_not_loose() {
        // Встроенные паки (resources.zip и datapacks внутри мира) едут
        // вместе с миром — отдельными ресурспаками не считаем.
        let path = temp_zip_path("embedded");
        let level = level_dat_bytes("1.20.1");
        let inner = inner_pack_bytes(true);
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("World/resources.zip", &inner),
                ("World/datapacks/dp/pack.mcmeta", b"{\"pack\":{}}"),
                ("World/datapacks/dp/data/x.mcfunction", b"say hi"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(pack.resource_packs.is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn datapack_folder_without_assets_is_not_a_pack() {
        // Папка с pack.mcmeta, но без assets/ — датапак, не ресурспак.
        let path = temp_zip_path("datapacklike");
        let level = level_dat_bytes("1.20.1");
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("LooseDP/pack.mcmeta", b"{\"pack\":{\"pack_format\":42}}"),
                ("LooseDP/data/x.mcfunction", b"say hi"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(pack.resource_packs.is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn already_placed_resourcepacks_are_not_loose() {
        // Пак уже под resourcepacks/ ставит оверлей — дубль не нужен.
        let path = temp_zip_path("placedrp");
        let level = level_dat_bytes("1.20.1");
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("resourcepacks/HP/pack.mcmeta", b"{\"pack\":{\"pack_format\":15}}"),
                ("resourcepacks/HP/assets/minecraft/x", b"x"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(pack.resource_packs.is_empty());
        assert!(pack.layers.contains(&"resourcepacks".to_string()));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn loose_zip_pack_is_detected_and_junk_zip_ignored() {
        let path = temp_zip_path("ziprp");
        let level = level_dat_bytes("1.20.1");
        let good = inner_pack_bytes(true);
        let junk = inner_pack_bytes(false);
        write_zip(
            &path,
            &[
                ("World/level.dat", &level),
                ("Extra/Cute.zip", &good),
                ("Extra/notes.zip", &junk),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.resource_packs.len(), 1);
        let rp = &pack.resource_packs[0];
        assert_eq!(rp.name, "Cute.zip");
        assert_eq!(rp.source, "Extra/Cute.zip");
        assert_eq!(rp.kind, ResourcePackKind::Zip);
        assert_eq!(rp.description.as_deref(), Some("Cute pack"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn pack_under_wrapper_is_detected() {
        // Мир и пак под общей обёрткой: пак — сосед мира, не контейнер.
        let path = temp_zip_path("wrapperrp");
        let level = level_dat_bytes("1.18.2");
        write_zip(
            &path,
            &[
                ("Pack/World/level.dat", &level),
                ("Pack/MyRP/pack.mcmeta", b"{\"pack\":{\"pack_format\":15}}"),
                ("Pack/MyRP/assets/minecraft/x", b"x"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert_eq!(pack.world_prefix, "Pack/World/");
        assert_eq!(pack.resource_packs.len(), 1);
        assert_eq!(pack.resource_packs[0].source, "Pack/MyRP/");
        assert_eq!(pack.resource_packs[0].name, "MyRP");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn content_root_itself_is_not_a_pack() {        // pack.mcmeta + assets/ в самом корне контента (обёртка-контейнер
        // с миром внутри) — не пак, иначе мир уехал бы в resourcepacks.
        let path = temp_zip_path("rootpack");
        let level = level_dat_bytes("1.18.2");
        write_zip(
            &path,
            &[
                ("Pack/World/level.dat", &level),
                ("Pack/pack.mcmeta", b"{\"pack\":{\"pack_format\":15}}"),
                ("Pack/assets/minecraft/x", b"x"),
            ],
        );

        let pack = analyze_zip(&path).expect("analyze");
        assert!(pack.resource_packs.is_empty());
        std::fs::remove_file(&path).ok();
    }
}
