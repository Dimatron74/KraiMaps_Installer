//! Прямой деплой нормализованной модели в цель (§§6.11, 6.15 ТЗ).
//!
//! - Ванилла: только мир в `saves/` — всегда безопасно.
//! - Моды в новый инстанс: мир + jar + слои по местам.
//! - Моды в чужой инстанс (только после wipe-экрана): `mods/` целиком в
//!   бэкап, затем файлы карты; `config/` — замена с бэкапом заменяемых.
//! - Мир пишется атомарно: `<Имя>.__install` → rename; замена запрещена.
//! - Всё удалённое/заменённое — в `.kraimaps-backup/<дата>/`.

use crate::domain::{resolve_unique_name, InstallError, ResourcePackKind, ResourcePackSelection};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Событие прогресса для UI (пробрасывается в Tauri events командой).
///
/// Стадии шлют прогресс в **несжатых байтах**: `done_bytes`/`total_bytes` —
/// сколько байт этой стадии уже записано / всего к записи. Пропущенные
/// записи в total не входят, счётчик монотонно растёт 0 → total, поэтому
/// процент честно отражает объём работы, а не число записей в архиве.
///
/// Сквозной прогресс всего деплоя (`overall_*`, тоже байты) проставляет
/// обёртка `commands::deploy_into`: сами стадии его не знают и шлют `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageEvent {
    pub stage: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current_file: String,
    #[serde(default)]
    pub overall_done: Option<u64>,
    #[serde(default)]
    pub overall_total: Option<u64>,
    /// Сырая строка лога установщика загрузчика. В заголовок идёт
    /// `current_file` (переведённая в человеческие слова фаза), а сюда —
    /// необработанная строка для раскрываемого «подробнее».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// План деплоя: его собирает UI из `PackAnalysis` + выборов пользователя.
/// Деплой сам перевыводит наборы из zip (анализ — для показа, не для записи).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployPlan {
    pub zip_path: PathBuf,
    pub world_prefix: String,
    pub world_name: String,
    /// Корень контента из анализа (`""` для плоских архивов).
    pub content_prefix: String,
    /// Alias `overrides/` из анализа (упакованный модпак).
    pub overrides_alias: bool,
    pub layers: Vec<String>,
    /// Ставить ли моды (из `PackAnalysis.is_modded`).
    pub deploy_mods: bool,
    /// Модовая карта в существующий чужой инстанс (wipe-экран подтверждён).
    pub wipe_mods: bool,
    /// Включённые пользователем loose-ресурспаки (подмножество
    /// `PackAnalysis.resource_packs` с галочек экрана проверки).
    #[serde(default)]
    pub resource_packs: Vec<ResourcePackSelection>,
}

/// Итог деплоя для экрана «Готово» и «Подробнее».
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeployReport {
    pub world_dir_name: String,
    pub mods_written: Vec<String>,
    pub backed_up: Vec<String>,
    pub skipped_identical: Vec<String>,
    /// Финальные имена ресурспаков в `resourcepacks/` (папки и zip).
    #[serde(default)]
    pub resource_packs_written: Vec<String>,
}

impl StageEvent {
    /// Событие стадии без сквозного прогресса (его проставляет обёртка
    /// деплоя; точечные стадии вроде провижининга шлют как есть).
    pub fn new(stage: &str, done_bytes: u64, total_bytes: u64, current_file: String) -> Self {
        Self {
            stage: stage.to_string(),
            done_bytes,
            total_bytes,
            current_file,
            overall_done: None,
            overall_total: None,
            detail: None,
        }
    }

    /// Событие с сырой строкой лога установщика для «подробнее».
    pub fn with_detail(mut self, line: &str) -> Self {
        self.detail = Some(line.to_string());
        self
    }
}

pub type Progress<'a> = &'a (dyn Fn(StageEvent) + Send + Sync);

/// Порция между событиями прогресса внутри большого файла: чаще — спам
/// в IPC, реже — полоса стоит на месте во время долгой записи.
const PROGRESS_CHUNK_BYTES: u64 = 256 * 1024;

/// Единица работы стадии: запись архива → относительный путь назначения.
struct StageWork {
    /// Индекс записи в zip (стабилен между открытиями одного файла).
    index: usize,
    /// Относительный путь назначения (для layers — уже с маппингом
    /// `datapacks/` → `saves/<мир>/`).
    rel: String,
    /// Сырое имя записи (для текста ошибки ZipSlip).
    raw: String,
    /// Несжатый размер записи — вес в прогрессе.
    size: u64,
    /// Строка для `current_file` в UI.
    label: String,
    /// Только для папок loose-ресурспаков: пустые папки сохраняем как есть.
    is_dir: bool,
}

fn open_zip(zip_path: &Path) -> Result<zip::ZipArchive<std::fs::File>, InstallError> {
    let file = std::fs::File::open(zip_path).map_err(InstallError::ZipOpen)?;
    zip::ZipArchive::new(file).map_err(|e| InstallError::ZipCorrupt(e.to_string()))
}

/// Пишет запись в файл назначения, считая байты. События — порциями
/// (`PROGRESS_CHUNK_BYTES`) и всегда в конце файла: большие region-файлы
/// тоже двигают полосу, а не вешают её на минуты.
fn copy_counted(
    entry: &mut impl Read,
    dest: &Path,
    progress: Progress,
    stage: &str,
    label: &str,
    done: &mut u64,
    total: u64,
) -> Result<(), InstallError> {
    let mut out = std::fs::File::create(dest)?;
    let mut buf = [0u8; 64 * 1024];
    let mut since_emit = 0u64;
    loop {
        let n = entry.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        *done += n as u64;
        since_emit += n as u64;
        if since_emit >= PROGRESS_CHUNK_BYTES {
            since_emit = 0;
            progress(StageEvent::new(stage, *done, total, label.to_string()));
        }
    }
    progress(StageEvent::new(stage, *done, total, label.to_string()));
    Ok(())
}

/// Итоги стадий в несжатых байтах — для сквозного прогресса всего деплоя.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageTotals {
    pub world: u64,
    pub mods: u64,
    pub resourcepacks: u64,
    pub layers: u64,
}

impl StageTotals {
    /// Сквозной знаменатель: только реально выполняемые стадии.
    pub fn overall(&self, deploy_mods: bool, has_resourcepacks: bool) -> u64 {
        self.world
            + self.layers
            + if deploy_mods { self.mods } else { 0 }
            + if has_resourcepacks {
                self.resourcepacks
            } else {
                0
            }
    }
}

/// Байтовые итоги всех стадий плана (один проход классификаторов,
/// без записи). Размеры не зависят от имён назначения, поэтому для layers
/// достаточно запрошенного имени мира из плана.
pub fn stage_totals(plan: &DeployPlan) -> Result<StageTotals, InstallError> {
    let (_, world) = world_work(&plan.zip_path, &plan.world_prefix)?;
    let (_, mods) = mods_work(&plan.zip_path, &plan.content_prefix)?;
    let mut resourcepacks = 0u64;
    for p in &plan.resource_packs {
        resourcepacks += match p.kind {
            ResourcePackKind::Folder => rp_folder_work(&plan.zip_path, &p.source)?.1,
            ResourcePackKind::Zip => rp_zip_work(&plan.zip_path, &p.source)?.size,
        };
    }
    let (excluded_prefixes, excluded_files) = rp_exclusions(&plan.resource_packs);
    let (_, layers) = layers_work(
        &plan.zip_path,
        &plan.world_prefix,
        &plan.content_prefix,
        plan.overrides_alias,
        &excluded_prefixes,
        &excluded_files,
        &plan.world_name,
    )?;
    Ok(StageTotals {
        world,
        mods,
        resourcepacks,
        layers,
    })
}

/// Источники loose-ресурспаков плана: папки — префиксы, zip — точные пути.
/// Оверлей слоёв пропускает их (иначе останется дубль пака в корне игры).
fn rp_exclusions(packs: &[ResourcePackSelection]) -> (Vec<String>, Vec<String>) {
    let mut excluded_prefixes = Vec::new();
    let mut excluded_files = Vec::new();
    for p in packs {
        match p.kind {
            ResourcePackKind::Folder => excluded_prefixes.push(p.source.clone()),
            ResourcePackKind::Zip => excluded_files.push(p.source.clone()),
        }
    }
    (excluded_prefixes, excluded_files)
}

/// Работа стадии «мир»: записи под `world_prefix` (папки пропускаем —
/// структуру восстанавливает создание родителей файлов).
fn world_work(zip_path: &Path, world_prefix: &str) -> Result<(Vec<StageWork>, u64), InstallError> {
    use crate::services::normalize::decode_name;
    let mut archive = open_zip(zip_path)?;
    let mut work = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let raw = decode_name(entry.name_raw());
        let Some(rel) = raw.strip_prefix(world_prefix).map(str::to_string) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let size = entry.size();
        total += size;
        work.push(StageWork {
            index: i,
            rel: rel.to_string(),
            raw,
            size,
            label: rel.to_string(),
            is_dir: false,
        });
    }
    Ok((work, total))
}

/// Работа стадии «моды»: всё дерево `mods/` (плоский архив:
/// `content_prefix=""`). Без фильтров по расширениям и вложенности.
fn mods_work(zip_path: &Path, content_prefix: &str) -> Result<(Vec<StageWork>, u64), InstallError> {
    use crate::services::normalize::decode_name;
    let mods_prefix = format!("{content_prefix}mods/");
    let mut archive = open_zip(zip_path)?;
    let mut work = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let raw = decode_name(entry.name_raw());
        let Some(rel) = mods_rel(&raw, content_prefix, &mods_prefix).map(str::to_string) else {
            continue;
        };
        if rel.is_empty() {
            continue; // сама папка mods/
        }
        let size = entry.size();
        total += size;
        work.push(StageWork {
            index: i,
            rel: rel.to_string(),
            raw,
            size,
            label: rel.to_string(),
            is_dir: false,
        });
    }
    Ok((work, total))
}

/// Работа одного loose-ресурспака (папки): записи под `source`.
/// Пустые папки сохраняем как есть (`is_dir`), остальное — файлами.
fn rp_folder_work(zip_path: &Path, source: &str) -> Result<(Vec<StageWork>, u64), InstallError> {
    use crate::services::normalize::decode_name;
    let mut archive = open_zip(zip_path)?;
    let mut work = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        let raw = decode_name(entry.name_raw());
        let Some(rel) = raw.strip_prefix(source).map(str::to_string) else {
            continue;
        };
        if rel.is_empty() {
            continue; // сама папка пака
        }
        let size = if entry.is_dir() { 0 } else { entry.size() };
        total += size;
        work.push(StageWork {
            index: i,
            rel: rel.to_string(),
            raw,
            size,
            label: rel.to_string(),
            is_dir: entry.is_dir(),
        });
    }
    Ok((work, total))
}

/// Работа одного loose-ресурспака (zip-файла): ровно одна запись.
fn rp_zip_work(zip_path: &Path, source: &str) -> Result<StageWork, InstallError> {
    use crate::services::normalize::decode_name;
    let mut archive = open_zip(zip_path)?;
    let idx = (0..archive.len())
        .find(|&i| {
            archive
                .by_index(i)
                .map(|e| decode_name(e.name_raw()) == source)
                .unwrap_or(false)
        })
        .ok_or_else(|| InstallError::ZipCorrupt(format!("ресурспак не найден в архиве: {source}")))?;
    let size = archive
        .by_index(idx)
        .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?
        .size();
    Ok(StageWork {
        index: idx,
        rel: source.to_string(),
        raw: source.to_string(),
        size,
        label: String::new(),
        is_dir: false,
    })
}

/// Работа стадии «слои»: всё из архива, кроме поддерева мира
/// (`deploy_world`), дерева `mods/` (`deploy_mods`) и loose-ресурспаков
/// (`deploy_resourcepacks`). `world_dir_name` влияет только на пути
/// назначения (`datapacks/` → `saves/<мир>/`), не на состав и размеры.
#[allow(clippy::too_many_arguments)]
fn layers_work(
    zip_path: &Path,
    world_prefix: &str,
    content_prefix: &str,
    overrides_alias: bool,
    excluded_prefixes: &[String],
    excluded_files: &[String],
    world_dir_name: &str,
) -> Result<(Vec<StageWork>, u64), InstallError> {
    use crate::services::normalize::{content_rel, decode_name};
    let mut archive = open_zip(zip_path)?;
    let mut work = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let raw = decode_name(entry.name_raw());
        if raw == world_prefix || raw.starts_with(world_prefix) {
            continue; // мир ставит deploy_world целиком
        }
        if excluded_prefixes
            .iter()
            .any(|p| raw == *p || raw.starts_with(p))
            || excluded_files.iter().any(|f| raw == *f)
        {
            continue; // loose-ресурспак ставит deploy_resourcepacks
        }
        let rel: String = match content_rel(&raw, content_prefix, overrides_alias) {
            Some(r) => {
                if r == "mods" || r.starts_with("mods/") {
                    continue;
                }
                r
            }
            None => {
                if raw.ends_with('/') || content_prefix.starts_with(&raw) {
                    continue;
                }
                if raw == "mods" || raw.starts_with("mods/") {
                    continue; // mods/ у корня при обёртке — за deploy_mods
                }
                raw.clone()
            }
        };
        // Корневые datapacks относятся к миру.
        let dest_rel = if rel.starts_with("datapacks/") {
            format!("saves/{world_dir_name}/{rel}")
        } else {
            rel.clone()
        };
        let size = entry.size();
        total += size;
        work.push(StageWork {
            index: i,
            rel: dest_rel.clone(),
            raw,
            size,
            label: dest_rel,
            is_dir: false,
        });
    }
    Ok((work, total))
}

/// Деплой мира: `saves/<Имя>/` (+ конфликт → `_v2`). Возвращает имя папки.
pub fn deploy_world(
    zip_path: &Path,
    world_prefix: &str,
    world_name: &str,
    saves_dir: &Path,
    progress: Progress,
) -> Result<String, InstallError> {
    std::fs::create_dir_all(saves_dir)?;
    let final_name = resolve_unique_name(world_name, &|n| saves_dir.join(n).exists());
    let staging = saves_dir.join(format!("{final_name}.__install"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    // Состав и вес стадии считаем заранее: прогресс идёт в байтах,
    // пропущенные записи в знаменатель не входят.
    let (work, total) = world_work(zip_path, world_prefix)?;
    let mut archive = open_zip(zip_path)?;
    let mut done = 0u64;
    for item in &work {
        let mut entry = archive
            .by_index(item.index)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        if entry.enclosed_name().is_none() {
            return Err(InstallError::ZipSlip(item.raw.clone()));
        }
        let dest = staging.join(&item.rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        copy_counted(&mut entry, &dest, progress, "world", &item.label, &mut done, total)?;
    }

    if !staging.join("level.dat").is_file() {
        std::fs::remove_dir_all(&staging).ok();
        return Err(InstallError::ZipCorrupt(
            "мир распаковался без level.dat".to_string(),
        ));
    }
    std::fs::rename(&staging, saves_dir.join(&final_name))?;
    Ok(final_name)
}

/// Деплой всего дерева `mods/` из архива (плоский архив: `content_prefix=""`).
/// Ставится ВСЁ: jar в корне, вложенные файлы и подпапки рекурсивно
/// (`mods/.index/*.toml`, `mods/pm/images/*.png` — данные сложных карт,
/// без них карта не работает). Фильтров по расширениям нет.
/// `wipe=true`: всё содержимое `mods/` сначала уезжает в бэкап (кроме
/// совпадающих по имени и хешу — они пропускаются).
pub fn deploy_mods(
    zip_path: &Path,
    content_prefix: &str,
    mods_dir: &Path,
    backup_dir: &Path,
    wipe: bool,
    progress: Progress,
) -> Result<DeployReport, InstallError> {
    std::fs::create_dir_all(mods_dir)?;
    let mut report = DeployReport::default();
    let mods_prefix = format!("{content_prefix}mods/");

    if wipe {
        std::fs::create_dir_all(backup_dir)?;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(mods_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        entries.sort();
        for existing in entries {
            let name = existing
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Совпадение по имени И хешу с входящим jar → оставить на месте.
            // Поиск по индексу: имена бывают не-UTF8, by_name с ними расходится.
            if !existing.is_file() {
                move_to_backup(&existing, backup_dir, &mut report)?;
                continue;
            }
            match find_entry_index(zip_path, &mods_prefix, &name)
                .or_else(|| {
                    (!content_prefix.is_empty())
                        .then(|| find_entry_index(zip_path, "mods/", &name))
                        .flatten()
                })
                .and_then(|idx| incoming_hash(zip_path, idx).ok().flatten())
            {
                Some(new_hash) if file_sha256(&existing)? == new_hash => {
                    report.skipped_identical.push(name);
                }
                _ => move_to_backup(&existing, backup_dir, &mut report)?,
            }
        }
    }

    let (work, total) = mods_work(zip_path, content_prefix)?;
    let mut archive = open_zip(zip_path)?;
    let mut done = 0u64;
    for item in &work {
        let mut entry = archive
            .by_index(item.index)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        let rel = item.rel.as_str();
        if entry.enclosed_name().is_none() {
            return Err(InstallError::ZipSlip(item.raw.clone()));
        }
        // Идентичные уже на месте (после wipe-штопора) — не трогаем,
        // но в прогрессе засчитываем: работа не требуется.
        let dest = mods_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if dest.is_file() {
            if let Some(new_hash) = incoming_hash(zip_path, item.index)? {
                if file_sha256(&dest)? == new_hash {
                    report.skipped_identical.push(rel.to_string());
                    done += item.size;
                    progress(StageEvent::new("mods", done, total, item.label.clone()));
                    continue;
                }
            }
            if wipe {
                std::fs::create_dir_all(&backup_dir)?;
                let rel_backup = backup_dir.join(rel);
                if let Some(parent) = rel_backup.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Бэкап уже может содержать этот путь (wipe-цикл выше):
                // тогда просто перезаписываем, история сохранена.
                if !rel_backup.exists() {
                    std::fs::rename(&dest, &rel_backup)?;
                    report.backed_up.push(rel.to_string());
                }
            }
        }
        copy_counted(&mut entry, &dest, progress, "mods", &item.label, &mut done, total)?;
        report.mods_written.push(rel.to_string());
    }
    report.mods_written.sort();
    report.mods_written.dedup();
    Ok(report)
}

/// Деплой loose-ресурспаков в `resourcepacks/` цели.
/// Папка ставится папкой как есть (Minecraft принимает и папки, и zip),
/// zip-запись — файлом без перепаковки. Конфликт — рядом с `_v2`
/// (как миры, чужое не перезаписываем никогда). Атомарно для папок:
/// `<Имя>.__install` → rename. Возвращает финальные имена.
pub fn deploy_resourcepacks(
    zip_path: &Path,
    packs: &[ResourcePackSelection],
    resourcepacks_dir: &Path,
    progress: Progress,
) -> Result<Vec<String>, InstallError> {
    use crate::domain::resolve_resourcepack_name;
    std::fs::create_dir_all(resourcepacks_dir)?;
    let mut written: Vec<String> = Vec::new();

    let mut archive = open_zip(zip_path)?;

    for pack in packs {
        match pack.kind {
            ResourcePackKind::Folder => {
                let final_name = resolve_resourcepack_name(&pack.name, pack.kind, &|n| {
                    resourcepacks_dir.join(n).exists()
                });
                let staging = resourcepacks_dir.join(format!("{final_name}.__install"));
                if staging.exists() {
                    std::fs::remove_dir_all(&staging)?;
                }
                std::fs::create_dir_all(&staging)?;
                let (work, total) = rp_folder_work(zip_path, &pack.source)?;
                let mut done = 0u64;
                for item in &work {
                    if item.is_dir {
                        // Пустые папки пака сохраняем как есть (вес нулевой).
                        std::fs::create_dir_all(staging.join(&item.rel))?;
                        continue;
                    }
                    let mut entry = archive
                        .by_index(item.index)
                        .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
                    if entry.enclosed_name().is_none() {
                        std::fs::remove_dir_all(&staging).ok();
                        return Err(InstallError::ZipSlip(item.raw.clone()));
                    }
                    let label = format!("{final_name}/{}", item.rel);
                    let dest = staging.join(&item.rel);
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    copy_counted(
                        &mut entry,
                        &dest,
                        progress,
                        "resourcepacks",
                        &label,
                        &mut done,
                        total,
                    )?;
                }
                std::fs::rename(&staging, resourcepacks_dir.join(&final_name))?;
                written.push(final_name);
            }
            ResourcePackKind::Zip => {
                let final_name = resolve_resourcepack_name(&pack.name, pack.kind, &|n| {
                    resourcepacks_dir.join(n).exists()
                });
                let item = rp_zip_work(zip_path, &pack.source)?;
                let mut entry = archive
                    .by_index(item.index)
                    .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
                if entry.enclosed_name().is_none() {
                    return Err(InstallError::ZipSlip(pack.source.clone()));
                }
                let dest = resourcepacks_dir.join(&final_name);
                let mut done = 0u64;
                copy_counted(
                    &mut entry,
                    &dest,
                    progress,
                    "resourcepacks",
                    &final_name,
                    &mut done,
                    item.size,
                )?;
                written.push(final_name);
            }
        }
    }
    written.sort();
    Ok(written)
}

/// Деплой оверлея: всё из архива, кроме поддерева мира
/// (его ставит deploy_world) и дерева `mods/` (ею владеет deploy_mods).
/// Без исключений: техмусора, «неизвестных» и внеобёрточных файлов больше
/// нет — кастомные скрипты/папки/ресурсы/тексты автора едут как есть,
/// с сохранением структуры. Файлы вне корня контента (напр.txt у корня
/// при папке-обёртке) ставятся по полному пути из архива.
/// Существующие файлы заменяются с предварительным бэкапом, полной очистки
/// нет. Корневые `datapacks/` — внутрь мира.
pub fn deploy_layers(
    zip_path: &Path,
    world_prefix: &str,
    content_prefix: &str,
    overrides_alias: bool,
    game_root: &Path,
    world_dir_name: &str,
    // Источники loose-ресурспаков из плана: их ставит deploy_resourcepacks
    // в `resourcepacks/`, оверлей их пропускает (иначе останется дубль
    // `<Пак>/` в корне игры). Папки — префиксы, zip — точные пути записей.
    excluded_prefixes: &[String],
    excluded_files: &[String],
    progress: Progress,
) -> Result<DeployReport, InstallError> {
    let mut report = DeployReport::default();
    // Один корень бэкапа на весь оверлей (считаем раз — в имени секунды).
    let backup_dir = backup_dir_for(game_root);

    let (work, total) = layers_work(
        zip_path,
        world_prefix,
        content_prefix,
        overrides_alias,
        excluded_prefixes,
        excluded_files,
        world_dir_name,
    )?;
    let mut archive = open_zip(zip_path)?;
    let mut done = 0u64;
    for item in &work {
        let mut entry = archive
            .by_index(item.index)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        if entry.enclosed_name().is_none() {
            return Err(InstallError::ZipSlip(item.raw.clone()));
        }
        let dest_rel = item.rel.as_str();
        let dest = game_root.join(dest_rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if dest.is_file() {
            // Замена с бэкапом (политика config из §5.3).
            std::fs::create_dir_all(&backup_dir)?;
            let rel_backup = backup_dir.join(dest_rel);
            if let Some(parent) = rel_backup.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if !rel_backup.exists() {
                std::fs::copy(&dest, &rel_backup)?;
                report.backed_up.push(dest_rel.to_string());
            }
        }
        copy_counted(
            &mut entry,
            &dest,
            progress,
            "layers",
            &item.label,
            &mut done,
            total,
        )?;
    }
    Ok(report)
}

/// Корень бэкапа для одного деплоя: `<game_root>/.kraimaps-backup/<дата>/`.
pub fn backup_dir_for(game_root: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    game_root.join(".kraimaps-backup").join(stamp)
}

fn move_to_backup(path: &Path, backup_dir: &Path, report: &mut DeployReport) -> Result<(), InstallError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let dest = backup_dir.join(&name);
    std::fs::rename(path, &dest)?;
    report.backed_up.push(name);
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, InstallError> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 1024 * 64];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Путь записи относительно `mods/`: сначала под корнем контента, затем
/// (при папке-обёртке) `mods/` у корня архива рядом с обёрткой.
/// Возвращает `None`, если запись не относится к дереву модов.
fn mods_rel<'a>(raw: &'a str, content_prefix: &str, mods_prefix: &str) -> Option<&'a str> {
    if let Some(rel) = raw.strip_prefix(mods_prefix) {
        return Some(rel);
    }
    if !content_prefix.is_empty() {
        if let Some(rel) = raw.strip_prefix("mods/") {
            return Some(rel);
        }
    }
    None
}

/// Индекс записи `<mods_prefix><name>` в архиве (линейный поиск по
/// декодированным именам — имен бывает немного, а `by_name` расходится
/// с декодером на не-UTF8 архивах).
fn find_entry_index(zip_path: &Path, mods_prefix: &str, name: &str) -> Option<usize> {
    let file = std::fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let want = format!("{mods_prefix}{name}");
    (0..archive.len()).find(|&i| {
        archive
            .by_index(i)
            .map(|e| crate::services::normalize::decode_name(e.name_raw()) == want)
            .unwrap_or(false)
    })
}

/// SHA-256 записи по индексу внутри zip (для пропуска идентичных).
fn incoming_hash(zip_path: &Path, index: usize) -> Result<Option<String>, InstallError> {
    use sha2::Digest;
    let file = std::fs::File::open(zip_path).map_err(InstallError::ZipOpen)?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
    let mut entry = archive
        .by_index(index)
        .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 1024 * 64];
    loop {
        let n = entry
            .read(&mut buf)
            .map_err(|e| InstallError::ZipCorrupt(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    ))
}

    #[cfg(test)]
    mod tests {
    /// Читает файл из game/.kraimaps-backup/<stamp>/rel (штамп неизвестен).
    fn read_backup_file(game: &Path, rel: &str) -> Vec<u8> {
        // Бэкап лежит в game/.kraimaps-backup/<stamp>/rel — штамп неизвестен.
        let base = game.join(".kraimaps-backup");
        let stamp = std::fs::read_dir(&base)
            .expect("backup root")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .expect("backup stamp dir");
        std::fs::read(stamp.join(rel)).expect("backup file")
    }
    use super::*;
    use std::io::Write;

    fn temp_root(tag: &str) -> crate::test_utils::TempDir {
        crate::test_utils::TempDir::new(&format!("mapinstaller-deploy-{tag}"))
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start");
            zip.write_all(data).expect("write");
        }
        zip.finish().expect("finish");
    }

    fn noop_progress(_: StageEvent) {}

    /// Регрессия «полоса в начале при 78%»: прогресс стадий идёт в несжатых
    /// байтах (а не в номерах записей), монотонно 0 → total. Тысяча мелких
    /// datapack-файлов больше не перевешивает один большой region-файл.
    #[test]
    fn world_progress_is_byte_based_and_monotonic() {
        let root = temp_root("progress");
        let zip = root.join("map.zip");
        let big = vec![0xABu8; 300_000];
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake-level"),
                ("W/datapacks/a.mcfunction", b"x"),
                ("W/datapacks/b.mcfunction", b"y"),
                ("W/region/r.mca", &big),
            ],
        );
        let saves = root.join("saves");
        let events = std::sync::Mutex::new(Vec::new());
        let name = deploy_world(&zip, "W/", "W", &saves, &|ev: StageEvent| {
            events.lock().expect("events").push(ev);
        })
        .expect("deploy");
        assert_eq!(name, "W");
        let events = events.into_inner().expect("events");
        assert!(!events.is_empty(), "события прогресса обязаны быть");
        let expected_total =
            (b"fake-level".len() + 1 + 1 + big.len()) as u64;
        let last = events.last().expect("last");
        assert_eq!(last.total_bytes, expected_total);
        assert_eq!(last.done_bytes, expected_total);
        let mut prev = 0u64;
        for ev in &events {
            assert_eq!(ev.stage, "world");
            assert_eq!(ev.total_bytes, expected_total, "знаменатель фиксирован");
            assert!(ev.done_bytes >= prev, "счётчик только растёт");
            assert!(ev.done_bytes <= expected_total);
            prev = ev.done_bytes;
        }
        // Три мелких файла дают < 1% — большой файл решает.
        let small_done = (b"fake-level".len() + 2) as u64;
        assert!(
            small_done * 100 / expected_total < 1,
            "мелочь не должна двигать полосу"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// Сквозные итоги совпадают с суммой стадий: overall-знаменатель честный.
    #[test]
    fn stage_totals_match_deploy_stages() {
        let root = temp_root("totals");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake-level"),
                ("mods/a.jar", b"jar-bytes"),
                ("options.txt", b"gfx"),
            ],
        );
        let plan = DeployPlan {
            zip_path: zip.clone(),
            world_prefix: "W/".to_string(),
            world_name: "W".to_string(),
            content_prefix: String::new(),
            overrides_alias: false,
            layers: Vec::new(),
            deploy_mods: true,
            wipe_mods: false,
            resource_packs: Vec::new(),
        };
        let totals = stage_totals(&plan).expect("totals");
        assert_eq!(totals.world, b"fake-level".len() as u64);
        assert_eq!(totals.mods, b"jar-bytes".len() as u64);
        assert_eq!(totals.resourcepacks, 0);
        assert_eq!(totals.layers, b"gfx".len() as u64);
        assert_eq!(
            totals.overall(true, false),
            (b"fake-level".len() + b"jar-bytes".len() + b"gfx".len()) as u64
        );
        assert_eq!(
            totals.overall(false, false),
            (b"fake-level".len() + b"gfx".len()) as u64
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn world_deploys_and_conflict_goes_side_by_side() {
        let root = temp_root("world");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("MyWorld/level.dat", b"fake-level"),
                ("MyWorld/region/r.mca", b"fake-region"),
                ("MyWorld/session.lock", b"junk"),
            ],
        );
        let saves = root.join("saves");

        let first = deploy_world(&zip, "MyWorld/", "MyWorld", &saves, &noop_progress)
            .expect("deploy");
        assert_eq!(first, "MyWorld");
        assert!(saves.join("MyWorld/level.dat").is_file());
        // deploy-all без исключений: session.lock тоже едет (безвреден,
        // Minecraft перезапишет при запуске).
        assert!(saves.join("MyWorld/session.lock").is_file());

        // Конфликт: рядом, замена запрещена.
        let second = deploy_world(&zip, "MyWorld/", "MyWorld", &saves, &noop_progress)
            .expect("deploy");
        assert_eq!(second, "MyWorld_v2");
        assert!(saves.join("MyWorld/level.dat").is_file()); // оригинал цел
        assert!(saves.join("MyWorld_v2/level.dat").is_file());
        assert!(
            !saves.join("MyWorld.__install").exists(),
            "staging вычищен"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn root_settings_deploy_with_backup() {
        let root = temp_root("rootfiles");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake"),
                ("options.txt", b"author-gfx"),
            ],
        );
        let game = root.join("game");
        std::fs::create_dir_all(&game).expect("game");
        std::fs::write(game.join("options.txt"), b"user-gfx").expect("user opts");

        deploy_layers(
            &zip,
            "W/",
            "",
            false,
            &game,
            "W",
            &[],
            &[],
            &noop_progress,
        )
        .expect("layers");
        assert_eq!(
            std::fs::read(game.join("options.txt")).expect("read"),
            b"author-gfx"
        );
        assert_eq!(read_backup_file(&game, "options.txt"), b"user-gfx");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn custom_content_overlays_preserving_structure() {
        // Самописные скрипты/папки автора едут в корень игры, мир — нет.
        let root = temp_root("custom");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("MyMap/saves/World/level.dat", b"fake-level"),
                ("MyMap/scripts/cutscene.js", b"js"),
                ("MyMap/kubejs/startup.js", b"kb"),
                ("MyMap/icon.png", b"png"),
            ],
        );
        let game = root.join("game");
        let report = deploy_layers(
            &zip,
            "MyMap/saves/World/",
            "MyMap/",
            false,
            &game,
            "World",
            &[],
            &[],
            &noop_progress,
        )
        .expect("layers");
        assert!(game.join("scripts/cutscene.js").is_file());
        assert!(game.join("kubejs/startup.js").is_file());
        assert!(game.join("icon.png").is_file());
        // Поддерево мира не дублируется в корень игры...
        assert!(!game.join("saves").exists());
        // ...а мир ставится своим шагом.
        let world =
            deploy_world(&zip, "MyMap/saves/World/", "World", &game.join("saves"), &noop_progress)
                .expect("world");
        assert_eq!(world, "World");
        assert!(game.join("saves/World/level.dat").is_file());
        assert!(report.backed_up.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn wrapper_layout_deploys_from_content_root() {
        let root = temp_root("wrapper");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("Pack/World/level.dat", b"fake-level"),
                ("Pack/mods/a.jar", b"jar-bytes"),
                ("Pack/config/o.cfg", b"cfg"),
            ],
        );
        let mods = root.join("mods");
        let report =
            deploy_mods(&zip, "Pack/", &mods, &root.join("backup"), false, &noop_progress)
                .expect("deploy");
        assert_eq!(report.mods_written, ["a.jar"]);
        assert!(mods.join("a.jar").is_file());

        let game = root.join("game");
        deploy_layers(
            &zip,
            "Pack/World/",
            "Pack/",
            false,
            &game,
            "World",
            &[],
            &[],
            &noop_progress,
        )
        .expect("layers");
        assert!(game.join("config/o.cfg").is_file());
        // Мир из-под обёртки — по world_prefix, без изменений.
        let world = deploy_world(&zip, "Pack/World/", "World", &game.join("saves"), &noop_progress)
            .expect("world");
        assert_eq!(world, "World");
        assert!(game.join("saves/World/level.dat").is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mods_wipe_backs_up_and_skips_identical() {
        let root = temp_root("mods");
        let zip = root.join("map.zip");
        let new_jar = b"new-jar-bytes";
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake"),
                ("mods/newmod.jar", new_jar),
                ("mods/keepme.jar", b"same-bytes"),
            ],
        );
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).expect("mods");
        std::fs::write(mods.join("oldmod.jar"), b"old-bytes").expect("old");
        std::fs::write(mods.join("keepme.jar"), b"same-bytes").expect("same");
        let backup = root.join("backup");

        let report = deploy_mods(&zip, "", &mods, &backup, true, &noop_progress).expect("deploy");
        // Чужой jar уехал в бэкап, новый встал, идентичный пропущен.
        assert!(backup.join("oldmod.jar").is_file());
        assert!(mods.join("newmod.jar").is_file());
        assert!(mods.join("keepme.jar").is_file());
        assert!(!backup.join("keepme.jar").exists());
        assert!(report.mods_written.contains(&"newmod.jar".to_string()));
        assert!(report.skipped_identical.contains(&"keepme.jar".to_string()));
        assert!(report.backed_up.contains(&"oldmod.jar".to_string()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn layers_replace_config_with_backup() {
        let root = temp_root("layers");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake"),
                ("config/opts.txt", b"new-opts"),
                ("resourcepacks/rp.zip", b"fake-rp"),
            ],
        );
        let game = root.join("game");
        std::fs::create_dir_all(game.join("config")).expect("config");
        std::fs::write(game.join("config/opts.txt"), b"user-opts").expect("user cfg");

        deploy_layers(&zip, "W/", "", false, &game,
            "W",
            &[],
            &[],
            &noop_progress,
        )
        .expect("layers");
        assert_eq!(
            std::fs::read(game.join("config/opts.txt")).expect("read"),
            b"new-opts"
        );
        assert_eq!(read_backup_file(&game, "config/opts.txt"), b"user-opts");
        assert!(game.join("resourcepacks/rp.zip").is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nested_mods_files_deploy_recursively() {
        // Сложная карта: вложенные данные под mods/ обязаны встать.
        let root = temp_root("nestedmods");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake"),
                ("mods/core.jar", b"jar"),
                ("mods/.index/core.pw.toml", b"meta"),
                ("mods/pm/images/1.png", b"png"),
            ],
        );
        let mods = root.join("mods");
        let report =
            deploy_mods(&zip, "", &mods, &root.join("backup"), false, &noop_progress)
                .expect("deploy");
        assert!(mods.join("core.jar").is_file());
        assert!(mods.join(".index/core.pw.toml").is_file());
        assert!(mods.join("pm/images/1.png").is_file());
        assert!(report.mods_written.contains(&".index/core.pw.toml".to_string()));
        assert!(report
            .mods_written
            .contains(&"pm/images/1.png".to_string()));

        // Оверлей mods/ не дублирует — им владеет deploy_mods.
        let game = root.join("game");
        deploy_layers(&zip, "W/", "", false, &game, "W", &[], &[], &noop_progress).expect("layers");
        assert!(!game.join("mods").exists());
        assert!(!game.join(".index").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn files_outside_wrapper_deploy_by_full_path() {
        // Файлы рядом с обёрткой (УСЛОВИЯ, readme) ставятся по полному пути,
        // а не отсекаются.
        let root = temp_root("outside");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("Pack/World/level.dat", b"fake-level"),
                ("Pack/mods/a.jar", b"jar"),
                ("Pack/config/o.cfg", b"cfg"),
                ("readme.txt", b"stray at root"),
                (
                    "УСЛОВИЯ ИСПОЛЬЗОВАНИЯ.txt",
                    "условия".as_bytes(),
                ),
            ],
        );
        let game = root.join("game");
        deploy_layers(
            &zip,
            "Pack/World/",
            "Pack/",
            false,
            &game,
            "World",
            &[],
            &[],
            &noop_progress,
        )
        .expect("layers");
        assert!(game.join("config/o.cfg").is_file());
        assert!(game.join("readme.txt").is_file());
        assert!(game
            .join("УСЛОВИЯ ИСПОЛЬЗОВАНИЯ.txt")
            .is_file());
        // Обёртка сама папкой не переезжает.
        assert!(!game.join("Pack").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    fn folder_selection(name: &str, source: &str) -> ResourcePackSelection {
        ResourcePackSelection {
            source: source.to_string(),
            name: name.to_string(),
            kind: ResourcePackKind::Folder,
        }
    }

    fn zip_selection(name: &str, source: &str) -> ResourcePackSelection {
        ResourcePackSelection {
            source: source.to_string(),
            name: name.to_string(),
            kind: ResourcePackKind::Zip,
        }
    }

    /// Внутренний zip-пак для тестов (корень: pack.mcmeta + assets/).
    fn inner_pack_bytes() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            zip.start_file("pack.mcmeta", zip::write::SimpleFileOptions::default())
                .expect("mcmeta");
            zip.write_all(br#"{"pack":{"pack_format":15}}"#)
                .expect("write");
            zip.start_file("assets/minecraft/x", zip::write::SimpleFileOptions::default())
                .expect("asset");
            zip.write_all(b"x").expect("write");
            zip.finish().expect("finish");
        }
        buf.into_inner()
    }

    #[test]
    fn resourcepack_folder_deploys_to_resourcepacks_without_root_duplicate() {
        // MOTD2-кейс: loose-папка едет в resourcepacks/, дубля в корне игры нет.
        let root = temp_root("looserp");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake-level"),
                ("RP/pack.mcmeta", b"{\"pack\":{}}"),
                ("RP/assets/minecraft/x", b"x"),
                ("RP/pack.png", b"png"),
                ("options.txt", b"gfx"),
            ],
        );
        let game = root.join("game");
        let packs = vec![folder_selection("RP", "RP/")];

        let written = deploy_resourcepacks(
            &zip,
            &packs,
            &game.join("resourcepacks"),
            &noop_progress,
        )
        .expect("rps");
        assert_eq!(written, ["RP"]);
        assert!(game.join("resourcepacks/RP/pack.mcmeta").is_file());
        assert!(game.join("resourcepacks/RP/assets/minecraft/x").is_file());
        assert!(game.join("resourcepacks/RP/pack.png").is_file());
        assert!(!game.join("resourcepacks/RP.__install").exists());

        // Оверлей с исключениями: пак не дублируется, остальное едет.
        deploy_layers(
            &zip,
            "W/",
            "",
            false,
            &game,
            "W",
            &["RP/".to_string()],
            &[],
            &noop_progress,
        )
        .expect("layers");
        assert!(!game.join("RP").exists());
        assert!(game.join("options.txt").is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resourcepack_conflict_goes_side_by_side() {
        // Конфликт в resourcepacks/: рядом с _v2, чужое не трогаем.
        let root = temp_root("rpconflict");
        let zip = root.join("map.zip");
        write_zip(
            &zip,
            &[
                ("W/level.dat", b"fake-level"),
                ("RP/pack.mcmeta", b"{\"pack\":{}}"),
                ("RP/assets/x", b"x"),
            ],
        );
        let rps = root.join("resourcepacks");
        std::fs::create_dir_all(rps.join("RP")).expect("existing");

        let written =
            deploy_resourcepacks(&zip, &[folder_selection("RP", "RP/")], &rps, &noop_progress)
                .expect("rps");
        assert_eq!(written, ["RP_v2"]);
        assert!(rps.join("RP_v2/pack.mcmeta").is_file());
        assert!(rps.join("RP").is_dir()); // чужое цело
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resourcepack_zip_deploys_as_file_with_conflict() {
        let root = temp_root("rpzip");
        let zip = root.join("map.zip");
        let inner = inner_pack_bytes();
        write_zip(
            &zip,
            &[("W/level.dat", b"fake-level"), ("Extra/Cute.zip", &inner)],
        );
        let rps = root.join("resourcepacks");

        let written = deploy_resourcepacks(
            &zip,
            &[zip_selection("Cute.zip", "Extra/Cute.zip")],
            &rps,
            &noop_progress,
        )
        .expect("rps");
        assert_eq!(written, ["Cute.zip"]);
        // Файл — валидный пак: внутри pack.mcmeta в корне.
        let data = std::fs::read(rps.join("Cute.zip")).expect("read");
        let mut pack = zip::ZipArchive::new(std::io::Cursor::new(data)).expect("zip");
        assert!(pack.by_name("pack.mcmeta").is_ok());

        // Конфликт zip: суффикс перед расширением.
        let again = deploy_resourcepacks(
            &zip,
            &[zip_selection("Cute.zip", "Extra/Cute.zip")],
            &rps,
            &noop_progress,
        )
        .expect("rps");
        assert_eq!(again, ["Cute_v2.zip"]);
        assert!(rps.join("Cute.zip").is_file());
        std::fs::remove_dir_all(&root).ok();
    }
}
