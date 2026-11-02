//! Слой совместимости «OptiFine»: выбор ветки, подготовка плана, скачивание
//! в staging и применение к свежей цели.
//!
//! OptiFine — не загрузчик, а патч, поэтому слой живёт отдельным типом, а не
//! в `LoaderKind`. Карта важнее слоя: сбой скачивания/применения non-fatal
//! (возвращается `CompatReport` с ошибкой), но ошибка подготовки новой цели
//! (провижининг загрузчика) остаётся фатальной и решается вызывающим кодом.

use crate::adapters::{modrinth_pack, optifine};
use crate::domain::{
    mc_version_known, mc_version_le, CompatFailure, CompatFile, CompatKind, CompatPlan,
    CompatReport, CompatSummary, GameTarget, InstallError, LoaderKind, OptifinePlan,
    COMPAT_PLAN_SCHEMA,
};
use crate::services::{deploy, http, normalize, provenance, versions};
use crate::services::compat_temp::CompatLease;
use crate::services::safe_fs::SafeDir;
use crate::services::work_budget::WorkBudget;
use futures_util::{StreamExt, TryStreamExt};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Одновременных скачиваний файлов сборки: Modrinth CDN держит 300 запросов
/// в минуту, четыре потока дают скорость, не упираясь в лимит.
const COMPAT_CONCURRENCY: usize = 4;

pub const PLAN_PREFIX: &str = "compat-plan-";
pub const PACK_PREFIX: &str = "compat-pack-";
pub const STAGE_PREFIX: &str = "compat-stage-";
const MAX_PLAN_BYTES: u64 = 8 * 1024 * 1024;

const FO_DISPLAY: &str = "Fabulously Optimized";
const OPTIFINE_DISPLAY: &str = "Forge + OptiFine";

/// Подготовка плана слоя. Сеть нужна только для резолва версии/файла:
/// тяжёлые загрузки начинаются на стадии `compat-download` деплоя.
pub async fn prepare_compat(
    mc_version: Option<&str>,
    is_modded: bool,
    temp_root: &CompatLease,
) -> Result<CompatSummary, InstallError> {
    let budget = WorkBudget::within(LAYER_DEADLINE);
    let _cancel = budget.cancel_on_drop();
    if is_modded {
        return Err(InstallError::CompatUnavailable(
            "карта содержит свои моды — слой совместимости не добавляется".to_string(),
        ));
    }
    let Some(mc) = mc_version.map(str::trim).filter(|v| !v.is_empty()) else {
        return Err(InstallError::CompatUnavailable(
            "не удалось определить версию Minecraft — слой совместимости не добавлен".to_string(),
        ));
    };
    if !mc_version_known(mc) {
        return Err(InstallError::CompatUnavailable(format!(
            "версия Minecraft «{mc}» не распознана — слой совместимости не добавлен"
        )));
    }
    if mc_version_le(mc, "1.16.4") {
        return prepare_optifine(mc, temp_root, None, &budget).await;
    }
    match prepare_fo(mc, temp_root, &budget).await {
        Ok(summary) => Ok(summary),
        Err(fo) => {
            let note = format!("Fabulously Optimized недоступен: {fo}");
            prepare_optifine(mc, temp_root, Some(note), &budget)
                .await
                .map_err(|of| {
                    InstallError::CompatUnavailable(format!(
                        "Fabulously Optimized: {fo}; OptiFine: {of}"
                    ))
                })
        }
    }
}

async fn prepare_fo(mc: &str, temp_root: &CompatLease, budget: &WorkBudget) -> Result<CompatSummary, InstallError> {
    let version = budget.network(modrinth_pack::resolve_fo_version(mc, budget)).await
        .map_err(|error| InstallError::CompatApply(error.to_string()))??;
    prepare_fo_version(mc, version, temp_root, budget).await
}

pub(crate) async fn prepare_fo_version(
    mc: &str,
    version: modrinth_pack::FoVersion,
    temp_root: &CompatLease,
    budget: &WorkBudget,
) -> Result<CompatSummary, InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    let pack_path = temp_root.file(Path::new(&format!("{PACK_PREFIX}{}.mrpack", new_id())))
        .map_err(|e| InstallError::CompatApply(format!("temp .mrpack: {e}")))?;
    let plan_path = temp_root.file(Path::new(&format!("{PLAN_PREFIX}{}.json", new_id())))
        .map_err(|e| InstallError::CompatApply(format!("temp плана: {e}")))?;
    let written = modrinth_pack::download_mrpack(&version, &pack_path, budget).await?;
    let (worker_pack, worker_plan, worker_temp, worker_mc, worker_budget) = (
        pack_path.clone(), plan_path.clone(), temp_root.clone(), mc.to_string(), budget.clone(),
    );
    let summary = budget.blocking(move || {
        let pack = read_pack_with_budget(&worker_pack, &worker_temp, &worker_budget)?;
        let parsed = read_and_parse_with_budget(&pack, &worker_mc, &worker_budget)?;
        let apply_bytes = modrinth_pack::apply_size_with_budget(&parsed.files, &pack, &worker_budget)?;
        let overrides = modrinth_pack::override_paths_with_budget(&pack, &worker_budget)?;
        let plan = CompatPlan {
            schema: COMPAT_PLAN_SCHEMA,
            kind: CompatKind::FabulouslyOptimized,
            mc_version: worker_mc,
            display_name: FO_DISPLAY.to_string(),
            version: version.version_number,
            loader: LoaderKind::Fabric,
            loader_version: parsed.loader_version,
            total_bytes: Some(parsed.total_bytes),
            mrpack_path: Some(worker_pack.to_string_lossy().into_owned()),
            pack_sha512: Some(version.sha512),
            pack_size: Some(written),
            files: parsed.files,
            overrides,
            protected_paths: Vec::new(),
            optifine: None,
            note: None,
        };
        save_plan_with_budget(&worker_plan, &worker_temp, &plan, &worker_budget)?;
        Ok::<_, InstallError>(summary_for(&plan, &worker_plan, Some(apply_bytes)))
    }).await.map_err(|error| InstallError::CompatApply(error.to_string()))??;
    budget.commit(|| {
        pack_path.keep();
        plan_path.keep();
        Ok(())
    }).map_err(|error| InstallError::CompatApply(error.to_string()))?;
    Ok(summary)
}

async fn prepare_optifine(
    mc: &str,
    temp_root: &CompatLease,
    note: Option<String>,
    budget: &WorkBudget,
) -> Result<CompatSummary, InstallError> {
    let resolved = budget.network(optifine::resolve_release(mc, budget)).await
        .map_err(|error| InstallError::CompatApply(error.to_string()))??;
    let plan = CompatPlan {
        schema: COMPAT_PLAN_SCHEMA,
        kind: CompatKind::OptifineForge,
        mc_version: mc.to_string(),
        display_name: OPTIFINE_DISPLAY.to_string(),
        version: resolved.release.file_name.clone(),
        loader: LoaderKind::Forge,
        loader_version: resolved.forge_version.clone(),
        total_bytes: None,
        mrpack_path: None,
        pack_sha512: None,
        pack_size: None,
        files: Vec::new(),
        overrides: Vec::new(),
        protected_paths: Vec::new(),
        optifine: Some(OptifinePlan {
            file_name: resolved.release.file_name,
            forge_version: resolved.forge_version,
        }),
        note,
    };
    let plan_path = temp_root.file(Path::new(&format!("{PLAN_PREFIX}{}.json", new_id())))
        .map_err(|e| InstallError::CompatApply(format!("temp плана: {e}")))?;
    let (worker_plan, worker_temp, worker_budget) = (plan_path.clone(), temp_root.clone(), budget.clone());
    let summary = budget.blocking(move || {
        save_plan_with_budget(&worker_plan, &worker_temp, &plan, &worker_budget)?;
        Ok::<_, InstallError>(summary_for(&plan, &worker_plan, None))
    }).await.map_err(|error| InstallError::CompatApply(error.to_string()))??;
    budget.commit(|| { plan_path.keep(); Ok(()) })
        .map_err(|error| InstallError::CompatApply(error.to_string()))?;
    Ok(summary)
}

#[cfg(test)]
fn read_and_parse(
    pack: &[u8],
    mc: &str,
) -> Result<modrinth_pack::ParsedPack, InstallError> {
    read_and_parse_with_budget(pack, mc, &WorkBudget::default())
}

fn read_and_parse_with_budget(pack: &[u8], mc: &str, budget: &WorkBudget) -> Result<modrinth_pack::ParsedPack, InstallError> {
    let raw = modrinth_pack::read_index_with_budget(pack, budget)?;
    let parsed = modrinth_pack::parse_index_with_budget(&raw, mc, budget)?;
    modrinth_pack::override_size_with_budget(pack, parsed.total_bytes, budget)?;
    Ok(parsed)
}

fn read_pack_with_budget(path: &Path, temp_root: &Path, budget: &WorkBudget) -> Result<Vec<u8>, InstallError> {
    require_in_temp(path, temp_root, PACK_PREFIX, ".mrpack")?;
    SafeDir::with_budget(temp_root, budget.clone())
        .and_then(|root| root.read_bounded(Path::new(path.file_name().unwrap_or_default()), modrinth_pack::MAX_MRPACK_BYTES))
        .map_err(|e| InstallError::CompatInvalid(format!(".mrpack: {e}")))
}

fn summary_for(plan: &CompatPlan, plan_path: &Path, apply_bytes: Option<u64>) -> CompatSummary {
    CompatSummary {
        kind: plan.kind,
        display_name: plan.display_name.clone(),
        version: plan.version.clone(),
        loader: plan.loader,
        loader_version: plan.loader_version.clone(),
        mc_version: plan.mc_version.clone(),
        total_bytes: plan.total_bytes,
        apply_bytes,
        files_count: plan.files.len(),
        plan_path: plan_path.to_string_lossy().into_owned(),
        note: plan.note.clone(),
    }
}

/// Чтение плана с повторной валидацией: план — данные, а не доверенный код.
/// Путь, URL, хеши, размеры и версии перепроверяются перед любой записью.
pub fn load_plan(plan_path: &Path, temp_root: &Path) -> Result<CompatPlan, InstallError> {
    load_plan_and_pack(plan_path, temp_root).map(|(plan, _)| plan)
}

fn load_plan_and_pack(plan_path: &Path, temp_root: &Path) -> Result<(CompatPlan, Option<Vec<u8>>), InstallError> {
    load_plan_with_budget(plan_path, temp_root, &WorkBudget::default())
}

fn load_plan_with_budget(plan_path: &Path, temp_root: &Path, budget: &WorkBudget) -> Result<(CompatPlan, Option<Vec<u8>>), InstallError> {
    require_in_temp(plan_path, temp_root, PLAN_PREFIX, ".json")?;
    let root = SafeDir::with_budget(temp_root, budget.clone())
        .map_err(|e| InstallError::CompatInvalid(format!("temp слоя: {e}")))?;
    let raw = root.read_bounded(Path::new(plan_path.file_name().unwrap_or_default()), MAX_PLAN_BYTES)
        .map_err(|e| InstallError::CompatInvalid(format!("план слоя: {e}")))?;
    let plan: CompatPlan = serde_json::from_slice(&raw)
        .map_err(|e| InstallError::CompatInvalid(format!("план слоя: {e}")))?;
    if plan.schema != COMPAT_PLAN_SCHEMA {
        return Err(InstallError::CompatInvalid(format!(
            "неподдерживаемая схема плана слоя: {}",
            plan.schema
        )));
    }
    if !mc_version_known(&plan.mc_version) {
        return Err(InstallError::CompatInvalid(format!(
            "в плане слоя неизвестная версия Minecraft: {}",
            plan.mc_version
        )));
    }
    let pack = match plan.kind {
        CompatKind::FabulouslyOptimized => Some(validate_fo_plan(&plan, temp_root, budget)?),
        CompatKind::OptifineForge => {
            validate_optifine_plan(&plan)?;
            None
        }
    };
    for path in &plan.protected_paths {
        budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
        let safe = protected_content_path(path)?;
        if safe != *path {
            return Err(InstallError::CompatInvalid(format!(
                "путь контента в плане не канонический: {path}"
            )));
        }
    }
    Ok((plan, pack))
}

/// Запомнить в плане пути контента карты: слой совместимости не перезаписывает
/// их — приоритет карты выше `overrides` сборки (в том числе при повторе).
pub fn protect_content_paths(
    plan_path: &Path,
    temp_root: &Path,
    paths: Vec<String>,
) -> Result<(), InstallError> {
    let mut plan = load_plan(plan_path, temp_root)?;
    for path in paths {
        plan.protected_paths.push(protected_content_path(&path)?);
    }
    plan.protected_paths.sort();
    plan.protected_paths.dedup();
    save_plan(plan_path, temp_root, &plan)
}

pub fn protect_target_content_paths(
    target: &GameTarget,
    plan_path: &Path,
    temp_root: &Path,
    paths: Vec<String>,
) -> Result<(), InstallError> {
    // Непредставимые назначения тоже сохраняются: их ошибка должна блокировать retry.
    if !provenance::protect_content(target, paths.clone()) {
        return Err(InstallError::CompatApply("цель не создана установщиком".into()));
    }
    protect_content_paths(plan_path, temp_root, paths)
}

// Это учёт назначений карты, не разрешение на запись: допустимы её имена и алиасы ОС.
fn protected_content_path(raw: &str) -> Result<String, InstallError> {
    let invalid = || InstallError::CompatInvalid(format!("небезопасный путь контента: {raw}"));
    if raw.is_empty() || raw.starts_with(['/', '\\'])
        || raw.chars().any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return Err(invalid());
    }
    let normalized = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/').filter(|part| !part.is_empty()) {
        let part = if cfg!(windows) { part.trim_end_matches(' ') } else { part };
        match part {
            "." => continue,
            ".." => {
                parts.pop().ok_or_else(invalid)?;
                continue;
            }
            _ => {}
        }
        let part = if cfg!(windows) { part.trim_end_matches(['.', ' ']) } else { part };
        if part.is_empty() {
            return Err(invalid());
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(invalid());
    }
    Ok(parts.join("/"))
}

fn save_plan(plan_path: &Path, temp_root: &Path, plan: &CompatPlan) -> Result<(), InstallError> {
    save_plan_with_budget(plan_path, temp_root, plan, &WorkBudget::default())
}

fn save_plan_with_budget(plan_path: &Path, temp_root: &Path, plan: &CompatPlan, budget: &WorkBudget) -> Result<(), InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    require_in_temp(plan_path, temp_root, PLAN_PREFIX, ".json")?;
    let raw = serde_json::to_vec(plan)
        .map_err(|e| InstallError::CompatApply(format!("план слоя: {e}")))?;
    if raw.len() as u64 > MAX_PLAN_BYTES {
        return Err(InstallError::CompatInvalid("план слоя превышает лимит размера".into()));
    }
    let root = SafeDir::with_budget(temp_root, budget.clone())
        .map_err(|e| InstallError::CompatApply(format!("temp слоя: {e}")))?;
    root.write_atomic(Path::new(plan_path.file_name().unwrap_or_default()), |output| {
        budget.copy(&mut std::io::Cursor::new(&raw), output).map(|_| ())
    })
        .map_err(|e| InstallError::CompatApply(format!("план слоя: {e}")))
}

fn validate_fo_plan(plan: &CompatPlan, temp_root: &Path, budget: &WorkBudget) -> Result<Vec<u8>, InstallError> {
    if plan.loader != LoaderKind::Fabric || !versions::is_exact_version(&plan.loader_version) {
        return Err(InstallError::CompatInvalid(
            "в плане слоя нет точной версии fabric-loader".to_string(),
        ));
    }
    let pack = plan
        .mrpack_path
        .as_deref()
        .ok_or_else(|| InstallError::CompatInvalid("в плане нет пути к .mrpack".to_string()))?;
    let pack = Path::new(pack);
    require_in_temp(pack, temp_root, PACK_PREFIX, ".mrpack")?;
    let size = plan
        .pack_size
        .ok_or_else(|| InstallError::CompatInvalid("в плане нет размера .mrpack".to_string()))?;
    let sha = plan
        .pack_sha512
        .as_deref()
        .ok_or_else(|| InstallError::CompatInvalid("в плане нет SHA-512 .mrpack".to_string()))?;
    if !is_hex(sha, 128) {
        return Err(InstallError::CompatInvalid(
            "некорректный SHA-512 .mrpack в плане".to_string(),
        ));
    }
    let pack = read_pack_with_budget(pack, temp_root, budget)?;
    if pack.len() as u64 != size {
        return Err(InstallError::CompatInvalid(format!(
            ".mrpack изменился: {} != {size}",
            pack.len()
        )));
    }
    use sha2::Digest;
    let mut hasher = sha2::Sha512::new();
    for bytes in pack.chunks(64 * 1024) {
        budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
        hasher.update(bytes);
    }
    let actual = http::hex(hasher.finalize());
    if !actual.eq_ignore_ascii_case(sha) {
        return Err(InstallError::CompatInvalid(
            "SHA-512 .mrpack не сошёлся".to_string(),
        ));
    }
    if plan.files.is_empty() {
        return Err(InstallError::CompatInvalid(
            "в плане слоя нет файлов сборки".to_string(),
        ));
    }
    let mut total = 0u64;
    let mut paths = HashSet::new();
    for file in &plan.files {
        budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
        let safe = modrinth_pack::safe_rel_path(&file.path).ok_or_else(|| {
            InstallError::CompatInvalid(format!("небезопасный путь в плане: {}", file.path))
        })?;
        if safe != file.path {
            return Err(InstallError::CompatInvalid(format!(
                "путь в плане не канонический: {}",
                file.path
            )));
        }
        if !paths.insert(modrinth_pack::path_key(&file.path)) {
            return Err(InstallError::CompatInvalid(format!("дублирующийся путь в плане: {}", file.path)));
        }
        if file.size == 0 || file.size > modrinth_pack::MAX_FILE_BYTES {
            return Err(InstallError::CompatInvalid(format!(
                "{}: размер вне лимитов",
                file.path
            )));
        }
        if !http::pack_url_allowed(&file.url) {
            return Err(InstallError::CompatInvalid(format!(
                "{}: недоверенный адрес скачивания",
                file.path
            )));
        }
        if !is_hex(&file.sha512, 128) {
            return Err(InstallError::CompatInvalid(format!(
                "{}: некорректный SHA-512",
                file.path
            )));
        }
        if let Some(sha1) = &file.sha1 {
            if !is_hex(sha1, 40) {
                return Err(InstallError::CompatInvalid(format!(
                    "{}: некорректный SHA-1",
                    file.path
                )));
            }
        }
        total = total.saturating_add(file.size);
        if total > modrinth_pack::MAX_TOTAL_BYTES {
            return Err(InstallError::CompatInvalid(
                "слой превышает лимит размера".to_string(),
            ));
        }
    }
    if plan.total_bytes != Some(total) {
        return Err(InstallError::CompatInvalid(
            "итоговый размер плана не совпал с суммой файлов".to_string(),
        ));
    }
    let parsed = read_and_parse_with_budget(&pack, &plan.mc_version, budget)?;
    if plan.files != parsed.files || plan.loader_version != parsed.loader_version {
        return Err(InstallError::CompatInvalid("план слоя не совпадает с проверенным индексом .mrpack".into()));
    }
    if plan.overrides != modrinth_pack::override_paths_with_budget(&pack, budget)? {
        return Err(InstallError::CompatInvalid("пути overrides не совпадают с проверенным .mrpack".into()));
    }
    for path in &plan.overrides {
        let safe = modrinth_pack::safe_rel_path(path).ok_or_else(|| {
            InstallError::CompatInvalid(format!("небезопасный путь overrides в плане: {path}"))
        })?;
        if safe != *path {
            return Err(InstallError::CompatInvalid(format!(
                "путь overrides в плане не канонический: {path}"
            )));
        }
    }
    Ok(pack)
}

fn validate_optifine_plan(plan: &CompatPlan) -> Result<(), InstallError> {
    if plan.loader != LoaderKind::Forge {
        return Err(InstallError::CompatInvalid(
            "в плане OptiFine не Forge".to_string(),
        ));
    }
    let of = plan.optifine.as_ref().ok_or_else(|| {
        InstallError::CompatInvalid("в плане нет данных OptiFine".to_string())
    })?;
    if !optifine::is_safe_file_name(&of.file_name) {
        return Err(InstallError::CompatInvalid(format!(
            "недопустимое имя файла OptiFine: {}",
            of.file_name
        )));
    }
    if !of.file_name.starts_with(&format!("OptiFine_{}_HD_U_", plan.mc_version)) {
        return Err(InstallError::CompatInvalid(
            "файл OptiFine предназначен для другой версии Minecraft".into(),
        ));
    }
    if plan.version != of.file_name {
        return Err(InstallError::CompatInvalid(
            "версия плана не совпадает с файлом OptiFine".to_string(),
        ));
    }
    if !optifine::is_full_version(&of.forge_version) || plan.loader_version != of.forge_version {
        return Err(InstallError::CompatInvalid(format!(
            "некорректная версия Forge: {}",
            of.forge_version
        )));
    }
    if plan.total_bytes.is_some() {
        return Err(InstallError::CompatInvalid(
            "у OptiFine размер заранее неизвестен".to_string(),
        ));
    }
    Ok(())
}

fn is_hex(raw: &str, len: usize) -> bool {
    raw.len() == len && raw.chars().all(|c| c.is_ascii_hexdigit())
}

fn require_in_temp(
    path: &Path,
    temp_root: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<(), InstallError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if !name.starts_with(prefix) || !name.ends_with(suffix) {
        return Err(InstallError::CompatInvalid(format!(
            "файл слоя вне нашего temp: {name}"
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| InstallError::CompatInvalid("у файла слоя нет каталога".to_string()))?;
    if parent != temp_root || !path.is_absolute() {
        return Err(InstallError::CompatInvalid(
            "файл слоя лежит вне приватного temp".to_string(),
        ));
    }
    Ok(())
}

/// `mods/` цели принадлежит плану: папки нет, она пуста или в ней только
/// файлы, которые кладёт сам слой (индекс `.mrpack`, overrides, jar OptiFine).
/// Так повтор слоя после успешного применения не упирается в собственные
/// файлы, а пользовательский мод по-прежнему блокирует установку слоя.
#[cfg(test)]
fn mods_owned_by_plan(plan: &CompatPlan, game_root: &Path) -> bool {
    mods_owned_with_budget(plan, game_root, &WorkBudget::default()).unwrap_or(false)
}

fn mods_owned_with_budget(plan: &CompatPlan, game_root: &Path, budget: &WorkBudget) -> std::io::Result<bool> {
    let root = SafeDir::with_budget(game_root, budget.clone())?;
    let mods = match root.open_subdir(Path::new("mods")) {
        Ok(mods) => mods,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    let mut owned: HashSet<String> = plan.files.iter().map(|file| file.path.as_str())
        .chain(plan.overrides.iter().map(String::as_str))
        .filter_map(|path| modrinth_pack::path_key(path).strip_prefix("mods/").map(str::to_string))
        .collect();
    if let Some(of) = &plan.optifine {
        owned.insert(modrinth_pack::path_key(&of.file_name));
    }
    let files = mods.files()?;
    for entry in files {
        let rel = entry.path.to_string_lossy().replace('\\', "/");
        if !owned.contains(&modrinth_pack::path_key(&rel)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Применение слоя. Никогда не возвращает `Err`: карта уже установлена,
/// поэтому любой сбой уходит в отчёт и показывается пользователю.
///
/// Слой допустим только в цель, созданную установщиком: существующий
/// инстанс пользователя получает лишь карту. Проверка — на backend'е,
/// фронт может ошибиться.
pub async fn apply_compat(
    target: &GameTarget,
    summary: &CompatSummary,
    plan_path: &Path,
    temp_root: &CompatLease,
    progress: deploy::ProgressFn,
) -> CompatReport {
    if !provenance::was_created(target) {
        return CompatReport::failed(
            Some(summary.kind),
            summary.display_name.clone(),
            summary.version.clone(),
            CompatFailure {
                code: InstallError::CompatApply(String::new()).code().to_string(),
                message: "слой совместимости ставится только в цель, созданную установщиком"
                    .to_string(),
                details: String::new(),
            },
        );
    }
    let budget = WorkBudget::within(LAYER_DEADLINE);
    let _cancel = budget.cancel_on_drop();
    let game_root = normalize::game_root(target);
    let (owned_path, owned_temp, owned_target, owned_game, worker_budget) = (
        plan_path.to_path_buf(), temp_root.clone(), target.clone(), game_root.clone(), budget.clone(),
    );
    let prepared = budget.blocking(move || {
        let (plan, pack) = load_plan_with_budget(&owned_path, &owned_temp, &worker_budget)?;
        let plan = bind_plan_to_target(plan, &owned_target)?;
        if !mods_owned_with_budget(&plan, &owned_game, &worker_budget)
            .map_err(|error| InstallError::CompatApply(error.to_string()))?
        {
            return Err(InstallError::CompatApply(
                "в папке mods/ есть посторонние файлы — слой ставится только в новый инстанс".into(),
            ));
        }
        Ok((plan, pack))
    }).await.map_err(|error| InstallError::CompatApply(error.to_string())).and_then(|result| result);
    let (plan, pack) = match prepared {
        Ok(plan) => plan,
        Err(e) => {
            return CompatReport::failed(
                Some(summary.kind),
                summary.display_name.clone(),
                summary.version.clone(),
                failure(&e),
            );
        }
    };
    match apply_plan_with_budget(&game_root, Arc::new(plan.clone()), pack.map(Arc::new), temp_root, progress, &budget).await {
        Ok(applied) => CompatReport {
            kind: Some(plan.kind),
            display_name: plan.display_name.clone(),
            version: plan.version.clone(),
            files_written: applied.files,
            jar_sha256: applied.jar_sha256,
            error: None,
        },
        Err(e) => CompatReport::failed(
            Some(plan.kind),
            plan.display_name.clone(),
            plan.version.clone(),
            failure(&e),
        ),
    }
}

fn bind_plan_to_target(mut plan: CompatPlan, target: &GameTarget) -> Result<CompatPlan, InstallError> {
    if plan.mc_version != target.minecraft_version
        || plan.loader != target.loader
        || Some(plan.loader_version.as_str()) != target.loader_version.as_deref()
    {
        return Err(InstallError::CompatInvalid(format!(
                "сборка требует Minecraft {} / {:?} {}; цель: {} / {:?} {}",
                plan.mc_version, plan.loader, plan.loader_version,
                target.minecraft_version, target.loader, target.loader_version.as_deref().unwrap_or("—"),
            )));
    }
    plan.protected_paths.extend(provenance::protected_paths(target));
    plan.protected_paths = plan.protected_paths.iter()
        .map(|path| protected_content_path(path))
        .collect::<Result<_, _>>()?;
    plan.protected_paths.sort();
    plan.protected_paths.dedup();
    Ok(plan)
}

pub fn failure(e: &InstallError) -> CompatFailure {
    CompatFailure {
        code: e.code().to_string(),
        message: e.to_string(),
        details: e.details(),
    }
}

/// Итог применения слоя: сколько файлов легло и SHA-256 jar OptiFine.
#[derive(Debug)]
struct Applied {
    files: usize,
    jar_sha256: Option<String>,
}

/// Общий дедлайн слоя: скачивание и применение вместе не должны длиться
/// дольше. Сумма лимитов по файлам этого не гарантирует — их до двадцати.
const LAYER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[cfg(test)]
async fn apply_plan_within(
    game_root: &Path,
    plan: &CompatPlan,
    pack: Option<&[u8]>,
    temp_root: &Path,
    progress: deploy::ProgressFn,
    deadline: std::time::Duration,
) -> Result<Applied, InstallError> {
    let budget = WorkBudget::within(deadline);
    let _cancel = budget.cancel_on_drop();
    let root = crate::test_utils::compat_root(temp_root);
    apply_plan_with_budget(game_root, Arc::new(plan.clone()), pack.map(|bytes| Arc::new(bytes.to_vec())), &root, progress, &budget).await
}

async fn apply_plan_with_budget(
    game_root: &Path,
    plan: Arc<CompatPlan>,
    pack: Option<Arc<Vec<u8>>>,
    temp_root: &CompatLease,
    progress: deploy::ProgressFn,
    budget: &WorkBudget,
) -> Result<Applied, InstallError> {
    budget.check().map_err(|error| InstallError::CompatApply(error.to_string()))?;
    let owner = temp_root.clone();
    let staging = budget.blocking(move || owner.staging()).await
        .map_err(|e| InstallError::CompatApply(format!("staging: {e}")))?
        .map_err(|e| InstallError::CompatApply(format!("staging: {e}")))?;
    stage_and_apply(game_root, plan, pack, staging, progress, budget).await
}

async fn stage_and_apply(
    game_root: &Path,
    plan: Arc<CompatPlan>,
    pack: Option<Arc<Vec<u8>>>,
    staging: CompatLease,
    progress: deploy::ProgressFn,
    budget: &WorkBudget,
) -> Result<Applied, InstallError> {
    let mut jar_sha256 = None;
    match plan.kind {
        CompatKind::FabulouslyOptimized => {
            let pack = pack.ok_or_else(|| {
                InstallError::CompatInvalid("в плане нет .mrpack".to_string())
            })?;
            let total = plan.total_bytes.unwrap_or(0);
            budget.network(download_pack_files(&plan.files, total, &staging, &*progress, budget)).await
                .map_err(|error| InstallError::CompatApply(error.to_string()))??;
            // Overrides кладём после файлов индекса: по спецификации они
            // перекрывают совпавшие пути.
            let (worker_stage, worker_budget) = (staging.clone(), budget.clone());
            budget.blocking(move || modrinth_pack::extract_overrides_with_budget(&pack, &worker_stage, &worker_budget)).await
                .map_err(|error| InstallError::CompatApply(error.to_string()))??;
        }
        CompatKind::OptifineForge => {
            let of = plan.optifine.as_ref().ok_or_else(|| {
                InstallError::CompatInvalid("в плане нет данных OptiFine".to_string())
            })?;
            let dest = staging.file(&Path::new("mods").join(&of.file_name))
                .map_err(|error| InstallError::CompatApply(error.to_string()))?;
            let label = format!("OptiFine · {}", of.file_name);
            progress(deploy::StageEvent::new("compat-download", 0, 0, label.clone()));
            let callback = |done: u64, total: u64| {
                progress(deploy::StageEvent::new(
                    "compat-download",
                    done,
                    total,
                    label.clone(),
                ));
            };
            jar_sha256 = Some(budget.network(optifine::download_jar_named(&of.file_name, &dest, Some(&callback), budget)).await
                .map_err(|error| InstallError::CompatApply(error.to_string()))??);
            dest.keep();
        }
    }
    let (game_root, worker_budget) = (game_root.to_path_buf(), budget.clone());
    let files = budget.blocking(move || {
        let result = apply_staging_with_budget(&staging, &game_root, &plan, &*progress, &worker_budget);
        drop(staging);
        result
    }).await.map_err(|error| InstallError::CompatApply(error.to_string()))??;
    Ok(Applied { files, jar_sha256 })
}

async fn download_pack_files(
    files: &[CompatFile],
    total: u64,
    staging: &CompatLease,
    progress: deploy::Progress<'_>,
    budget: &WorkBudget,
) -> Result<(), InstallError> {
    progress(deploy::StageEvent::new("compat-download", 0, total, FO_DISPLAY.into()));
    let stage_done = Arc::new(AtomicU64::new(0));
    futures_util::stream::iter(files.iter().cloned())
        .map(|file| {
            let stage_done = stage_done.clone();
            let staging = staging.clone();
            async move {
                let dest = staging.file(Path::new(&file.path))
                    .map_err(|error| InstallError::CompatApply(error.to_string()))?;
                let file_done = AtomicU64::new(0);
                let callback = |bytes: u64, _file_total: u64| {
                    let previous = file_done.swap(bytes, Ordering::Relaxed);
                    let delta = bytes.saturating_sub(previous);
                    let overall = stage_done.fetch_add(delta, Ordering::Relaxed) + delta;
                    progress(deploy::StageEvent::new(
                        "compat-download",
                        overall,
                        total,
                        format!("{FO_DISPLAY} · {}", file.path),
                    ));
                };
                http::download_owned(
                    &file.url,
                    &dest,
                    http::Expected {
                        size: Some(file.size),
                        sha512: Some(&file.sha512),
                        sha1: file.sha1.as_deref(),
                        max_bytes: modrinth_pack::MAX_FILE_BYTES,
                    },
                    &file.path,
                    Some(&callback),
                    http::DownloadPolicy::default(),
                    budget,
                )
                .await
                .map(|_| dest.keep())
            }
        })
        .buffer_unordered(COMPAT_CONCURRENCY)
        .try_collect::<Vec<()>>()
        .await
        .map(|_| ())
}

/// Перенос готового staging в цель. Существующие файлы уезжают в
/// `.kraimaps-backup/<дата>/` по общим правилам `deploy_layers`.
#[cfg(test)]
fn apply_staging(
    staging: &Path,
    game_root: &Path,
    plan: &CompatPlan,
    progress: deploy::Progress<'_>,
) -> Result<usize, InstallError> {
    apply_staging_with_budget(staging, game_root, plan, progress, &WorkBudget::default())
}

fn apply_staging_with_budget(
    staging: &Path,
    game_root: &Path,
    plan: &CompatPlan,
    progress: deploy::Progress<'_>,
    budget: &WorkBudget,
) -> Result<usize, InstallError> {
    let source = SafeDir::with_budget(staging, budget.clone())
        .map_err(|e| InstallError::CompatApply(format!("staging: {e}")))?;
    let target = SafeDir::with_budget(game_root, budget.clone())
        .map_err(|e| InstallError::CompatApply(format!("цель слоя: {e}")))?;
    let protected: HashSet<_> = plan.protected_paths.iter()
        .map(|path| modrinth_pack::path_key(path))
        .collect();
    let mut work: Vec<(String, u64)> = Vec::new();
    let mut total = 0u64;
    for entry in source.files()
        .map_err(|e| InstallError::CompatApply(format!("staging: {e}")))? {
        let rel = entry.path.to_string_lossy().replace('\\', "/");
        let rel = modrinth_pack::safe_rel_path(&rel).ok_or_else(|| {
            InstallError::CompatApply(format!("небезопасный путь слоя: {rel}"))
        })?;
        if is_protected(&rel, &protected) {
            continue;
        }
        total = total.saturating_add(entry.size);
        work.push((rel, entry.size));
    }

    let backup_dir = deploy::backup_dir_for(game_root);
    progress(deploy::StageEvent::new("compat-apply", 0, total, plan.display_name.clone()));
    let mut done = 0u64;
    let mut written = 0usize;
    for (rel, _size) in work {
        let existing = match target.open_file(Path::new(&rel)) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(InstallError::CompatApply(format!("{rel}: {error}"))),
        };
        if let Some(mut existing) = existing {
            let backup = backup_dir.join(&rel);
            let backup_rel = backup.strip_prefix(game_root)
                .map_err(|e| InstallError::CompatApply(format!("бэкап {rel}: {e}")))?;
            match target.open_file(backup_rel) {
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    target.write_atomic(backup_rel, |output| {
                        budget.copy(&mut existing, output).map(|_| ())
                    }).map_err(|e| InstallError::CompatApply(format!("бэкап {rel}: {e}")))?;
                }
                Err(error) => return Err(InstallError::CompatApply(format!("бэкап {rel}: {error}"))),
            }
        }
        let mut input = source.open_file(Path::new(&rel))
            .map_err(|e| InstallError::CompatApply(format!("{rel}: {e}")))?;
        target.write_atomic(Path::new(&rel), |output| {
            copy_local(&mut input, output, progress, &rel, &mut done, total, budget)
        }).map_err(|e| InstallError::CompatApply(format!("{rel}: {e}")))?;
        written += 1;
    }
    Ok(written)
}

fn is_protected(path: &str, protected: &HashSet<String>) -> bool {
    let key = modrinth_pack::path_key(path);
    let mut part = key.as_str();
    loop {
        if protected.contains(part) {
            return true;
        }
        let Some((parent, _)) = part.rsplit_once('/') else {
            return false;
        };
        part = parent;
    }
}

fn copy_local(
    input: &mut impl Read,
    output: &mut impl Write,
    progress: deploy::Progress<'_>,
    label: &str,
    done: &mut u64,
    total: u64,
    budget: &WorkBudget,
) -> std::io::Result<()> {
    let mut buf = [0u8; 64 * 1024];
    let mut copied = 0u64;
    loop {
        budget.check()?;
        let n = input.read(&mut buf)?;
        budget.check()?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        copied += n as u64;
        *done += n as u64;
        if copied >= 256 * 1024 {
            copied = 0;
            progress(deploy::StageEvent::new(
                "compat-apply",
                *done,
                total,
                label.to_string(),
            ));
        }
    }
    budget.check()?;
    output.flush()?;
    progress(deploy::StageEvent::new(
        "compat-apply",
        *done,
        total,
        label.to_string(),
    ));
    Ok(())
}

/// Удаление плана и связанного `.mrpack` (уход с «Готово»/`reset()`).
/// Принимает только наши файлы внутри системного temp; возвращает число
/// удалённых. Best-effort, не падает.
pub fn discard_compat(plan_path: &Path, temp_root: &Path) -> usize {
    if require_in_temp(plan_path, temp_root, PLAN_PREFIX, ".json").is_err() {
        return 0;
    }
    let Ok(root) = SafeDir::open(temp_root) else {
        return 0;
    };
    let plan_rel = Path::new(plan_path.file_name().unwrap_or_default());
    let mut removed = 0usize;
    if let Ok(raw) = root.read_bounded(plan_rel, MAX_PLAN_BYTES) {
        if let Ok(plan) = serde_json::from_slice::<CompatPlan>(&raw) {
            if let Some(pack) = plan.mrpack_path.as_deref() {
                let pack = Path::new(pack);
                if require_in_temp(pack, temp_root, PACK_PREFIX, ".mrpack").is_ok()
                    && root.remove_file(Path::new(pack.file_name().unwrap_or_default())).is_ok()
                {
                    removed += 1;
                }
            }
        }
    }
    if root.remove_file(plan_rel).is_ok() {
        removed += 1;
    }
    removed
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::TempDir;
    use std::path::PathBuf;

    fn file_entry(path: &str, url: &str, size: u64) -> CompatFile {
        CompatFile {
            path: path.to_string(),
            url: url.to_string(),
            size,
            sha512: "a".repeat(128),
            sha1: None,
        }
    }

    fn fo_index(files: &[CompatFile]) -> Vec<u8> {
        let files: Vec<_> = files.iter().map(|file| serde_json::json!({
            "path": file.path,
            "hashes": { "sha512": file.sha512 },
            "downloads": [file.url],
            "fileSize": file.size,
        })).collect();
        serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "files": files,
            "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.14.23" },
        })).expect("index")
    }

    fn fo_plan(temp_root: &Path, pack_bytes: &[u8]) -> CompatPlan {
        let pack = temp_root.join(format!("{PACK_PREFIX}test.mrpack"));
        let files = vec![file_entry(
            "mods/sodium.jar",
            "https://cdn.modrinth.com/data/a/versions/1/sodium.jar",
            100,
        )];
        crate::test_utils::write_zip(&pack, &[
            ("modrinth.index.json", &fo_index(&files)),
            ("pack.marker", pack_bytes),
        ]);
        let sha = http::sha512_file(&pack).expect("sha");
        let size = std::fs::metadata(&pack).expect("pack metadata").len();
        CompatPlan {
            schema: COMPAT_PLAN_SCHEMA,
            kind: CompatKind::FabulouslyOptimized,
            mc_version: "1.20.1".to_string(),
            display_name: FO_DISPLAY.to_string(),
            version: "5.4.1".to_string(),
            loader: LoaderKind::Fabric,
            loader_version: "0.14.23".to_string(),
            total_bytes: Some(100),
            mrpack_path: Some(pack.to_string_lossy().into_owned()),
            pack_sha512: Some(sha),
            pack_size: Some(size),
            files,
            overrides: Vec::new(),
            protected_paths: Vec::new(),
            optifine: None,
            note: None,
        }
    }

    fn write_plan_file(temp_root: &Path, plan: &CompatPlan) -> PathBuf {
        let path = temp_root.join(format!("{PLAN_PREFIX}test.json"));
        std::fs::write(&path, serde_json::to_vec(plan).expect("json")).expect("plan");
        path
    }

    fn optifine_plan() -> CompatPlan {
        CompatPlan {
            schema: COMPAT_PLAN_SCHEMA,
            kind: CompatKind::OptifineForge,
            mc_version: "1.12.2".into(),
            display_name: OPTIFINE_DISPLAY.into(),
            version: "OptiFine_1.12.2_HD_U_G5.jar".into(),
            loader: LoaderKind::Forge,
            loader_version: "14.23.5.2847".into(),
            total_bytes: None,
            mrpack_path: None,
            pack_sha512: None,
            pack_size: None,
            files: Vec::new(),
            overrides: Vec::new(),
            protected_paths: Vec::new(),
            optifine: Some(OptifinePlan {
                file_name: "OptiFine_1.12.2_HD_U_G5.jar".into(),
                forge_version: "14.23.5.2847".into(),
            }),
            note: None,
        }
    }

    async fn fo_preparation_fixture(parent: &Path, delay: std::time::Duration) -> (modrinth_pack::FoVersion, tokio::task::JoinHandle<()>) {
        let path = parent.join("source.mrpack");
        let files = [file_entry("mods/sodium.jar", "https://cdn.modrinth.com/data/a/sodium.jar", 100)];
        crate::test_utils::write_zip(&path, &[("modrinth.index.json", &fo_index(&files))]);
        let bytes = std::fs::read(&path).expect("fixture pack");
        let sha512 = http::sha512_file(&path).expect("fixture hash");
        let size = bytes.len() as u64;
        let (url, server) = crate::test_utils::serve_http_once(bytes, delay).await;
        (modrinth_pack::FoVersion { version_number: "5.4.1".into(), file_name: "FO.mrpack".into(),
            file_url: url, sha512, size }, server)
    }

    #[tokio::test]
    async fn expired_fo_preparation_cannot_publish_a_plan() {
        let source = TempDir::new("compat-prepare-expired-source");
        let parent = TempDir::new("compat-prepare-expired-temp");
        let temp = crate::test_utils::compat_root(&parent);
        let (version, server) = fo_preparation_fixture(&source, std::time::Duration::ZERO).await;
        let result = prepare_fo_version("1.20.1", version, &temp,
            &WorkBudget::within(std::time::Duration::ZERO)).await;
        server.abort();

        assert!(result.is_err(), "an expired preparation must not publish a usable plan");
        assert_eq!(std::fs::read_dir(&temp).expect("temp").count(), 1, "only the ownership marker may remain");
    }

    #[tokio::test]
    async fn fo_preparation_deadline_includes_download_and_plan_save() {
        let source = TempDir::new("compat-prepare-deadline-source");
        let parent = TempDir::new("compat-prepare-deadline-temp");
        let temp = crate::test_utils::compat_root(&parent);
        let (version, server) = fo_preparation_fixture(&source, std::time::Duration::from_millis(150)).await;
        let result = prepare_fo_version("1.20.1", version, &temp,
            &WorkBudget::within(std::time::Duration::from_millis(50))).await;
        server.abort();

        assert!(result.is_err(), "the preparation deadline must include all file work");
        crate::test_utils::wait_for_entry_count(&temp, 1).await;
    }

    #[tokio::test]
    async fn failed_fo_preparation_removes_the_downloaded_pack_without_publishing_a_plan() {
        use sha2::Digest;
        let parent = TempDir::new("compat-prepare-invalid-pack");
        let temp = crate::test_utils::compat_root(&parent);
        let body = b"not a ZIP archive".to_vec();
        let (url, server) = crate::test_utils::serve_http_once(body.clone(), std::time::Duration::ZERO).await;
        let version = modrinth_pack::FoVersion { version_number: "test".into(), file_name: "FO.mrpack".into(),
            file_url: url, size: body.len() as u64, sha512: http::hex(sha2::Sha512::digest(&body)) };

        let result = prepare_fo_version("1.20.1", version, &temp, &WorkBudget::default()).await;
        server.await.expect("server");

        assert_eq!(result.expect_err("invalid pack").code(), "E_COMPAT_INVALID");
        crate::test_utils::wait_for_entry_count(&temp, 1).await;
    }

    #[tokio::test]
    async fn cancelling_fo_preparation_removes_its_partial_pack_and_unpublished_plan() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let parent = TempDir::new("compat-prepare-cancelled");
        let temp = crate::test_utils::compat_root(&parent);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("server");
        let url = format!("http://{}/pack", listener.local_addr().expect("address"));
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("connection");
            let _ = stream.read(&mut [0u8; 4096]).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\npartial")
                .await.expect("partial response");
            let _ = sent.send(());
            let _ = stream.read(&mut [0u8; 1]).await;
        });
        let version = modrinth_pack::FoVersion { version_number: "test".into(), file_name: "FO.mrpack".into(),
            file_url: url, size: 1024, sha512: "a".repeat(128) };
        let worker_temp = temp.clone();
        let preparation = tokio::spawn(async move {
            let budget = WorkBudget::default();
            prepare_fo_version("1.20.1", version, &worker_temp, &budget).await
        });
        received.await.expect("response started");

        preparation.abort();
        assert!(preparation.await.expect_err("cancelled future").is_cancelled());
        server.abort();
        crate::test_utils::wait_for_entry_count(&temp, 1).await;
        assert!(!temp.join("compat-plan-unpublished.json").exists());
    }

    #[tokio::test]
    async fn apply_rejects_a_plan_for_other_target_versions_before_network_access() {
        use crate::domain::LauncherKind;
        let parent = TempDir::new("compat-plan-target-version");
        let temp = crate::test_utils::compat_root(&parent);
        let game = TempDir::new("compat-target-version-game");
        let plan = fo_plan(&temp, b"marker");
        let path = write_plan_file(&temp, &plan);
        let summary = summary_for(&plan, &path, plan.total_bytes);
        for (mc, loader, version) in [
            ("1.21.1", LoaderKind::Fabric, "0.14.23"),
            ("1.20.1", LoaderKind::Forge, "0.14.23"),
            ("1.20.1", LoaderKind::Fabric, "0.15.11"),
        ] {
            let target = GameTarget {
                launcher: LauncherKind::Official,
                path: game.to_path_buf(),
                name: format!("{mc}-{loader:?}-{version}"),
                minecraft_version: mc.into(),
                loader,
                loader_version: Some(version.into()),
            };
            provenance::register(&target);

            let report = tokio::time::timeout(std::time::Duration::from_millis(100),
                apply_compat(&target, &summary, &path, &temp, Arc::new(|_| {})))
                .await.expect("version mismatch must be rejected before downloading");

            assert_eq!(report.error.expect("wrong target versions").code, "E_COMPAT_INVALID");
            assert_eq!(std::fs::read_dir(&game).expect("game").count(), 0);
        }
    }

    #[test]
    fn plan_load_rejects_optifine_for_a_different_minecraft_version() {
        let temp = TempDir::new("compat-of-minecraft-mismatch");
        let mut plan = optifine_plan();
        plan.mc_version = "1.20.1".into();
        let path = write_plan_file(&temp, &plan);

        assert_eq!(load_plan(&path, &temp).expect_err("OptiFine must match Minecraft").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn plan_load_rejects_a_forge_version_not_selected_for_optifine() {
        let temp = TempDir::new("compat-of-forge-mismatch");
        let mut plan = optifine_plan();
        plan.loader_version = "14.23.5.2860".into();
        let path = write_plan_file(&temp, &plan);

        assert_eq!(load_plan(&path, &temp).expect_err("Forge must match the selected release").code(), "E_COMPAT_INVALID");
    }

    #[cfg(windows)]
    #[test]
    fn mods_guard_recognizes_owned_files_using_windows_case_rules() {
        let temp = TempDir::new("compat-mods-case-plan");
        let game = TempDir::new("compat-mods-case-game");
        let mut plan = fo_plan(&temp, b"marker");
        plan.files[0].path = "Mods/Sodium.JAR".into();
        std::fs::create_dir(game.join("mods")).expect("mods");
        std::fs::write(game.join("mods/sodium.jar"), b"jar").expect("mod");

        assert!(mods_owned_by_plan(&plan, &game));
    }

    #[tokio::test]
    async fn prepare_refuses_modded_unknown_and_missing_version() {
        let parent = TempDir::new("compat-prepare");
        let temp = crate::test_utils::compat_root(&parent);
        assert_eq!(
            prepare_compat(Some("1.20.1"), true, &temp)
                .await
                .expect_err("modded")
                .code(),
            "E_COMPAT_UNAVAILABLE"
        );
        assert_eq!(
            prepare_compat(None, false, &temp)
                .await
                .expect_err("no mc")
                .code(),
            "E_COMPAT_UNAVAILABLE"
        );
        assert_eq!(
            prepare_compat(Some("23w45a"), false, &temp)
                .await
                .expect_err("snapshot")
                .code(),
            "E_COMPAT_UNAVAILABLE"
        );
    }

    #[test]
    fn plan_load_accepts_valid_fo_and_builds_summary() {
        let temp = TempDir::new("compat-plan-ok");
        let plan = fo_plan(&temp, b"mrpack-bytes");
        let path = write_plan_file(&temp, &plan);
        let loaded = load_plan(&path, &temp).expect("valid plan");
        assert_eq!(loaded.kind, CompatKind::FabulouslyOptimized);
        let summary = summary_for(&loaded, &path, loaded.total_bytes);
        assert_eq!(summary.kind, CompatKind::FabulouslyOptimized);
        assert_eq!(summary.files_count, 1);
        assert_eq!(summary.total_bytes, Some(100));
        assert_eq!(summary.plan_path, path.to_string_lossy());
    }

    #[test]
    fn plan_load_rejects_tampering_and_foreign_paths() {
        let temp = TempDir::new("compat-plan-bad");
        let plan = fo_plan(&temp, b"mrpack-bytes");
        let path = write_plan_file(&temp, &plan);

        // Чужой путь и не наш префикс.
        let outside = TempDir::new("compat-plan-outside");
        assert_eq!(
            load_plan(&outside.join("compat-plan-x.json"), &temp)
                .expect_err("outside")
                .code(),
            "E_COMPAT_INVALID"
        );

        // Подменённый .mrpack (размер/хеш).
        std::fs::write(
            plan.mrpack_path.as_deref().expect("pack"),
            b"tampered",
        )
        .expect("tamper");
        assert_eq!(
            load_plan(&path, &temp).expect_err("tamper").code(),
            "E_COMPAT_INVALID"
        );
    }

    #[test]
    fn plan_load_rejects_unsafe_file_entries() {
        let temp = TempDir::new("compat-plan-entry");
        let mut plan = fo_plan(&temp, b"mrpack-bytes");
        plan.files[0].path = "../evil.jar".to_string();
        let path = write_plan_file(&temp, &plan);
        assert_eq!(
            load_plan(&path, &temp).expect_err("path").code(),
            "E_COMPAT_INVALID"
        );

        let mut plan = fo_plan(&temp, b"mrpack-bytes");
        plan.files[0].url = "https://evil.example/a.jar".to_string();
        let path = write_plan_file(&temp, &plan);
        assert_eq!(
            load_plan(&path, &temp).expect_err("url").code(),
            "E_COMPAT_INVALID"
        );

        let mut plan = fo_plan(&temp, b"mrpack-bytes");
        plan.total_bytes = Some(999);
        let path = write_plan_file(&temp, &plan);
        assert_eq!(
            load_plan(&path, &temp).expect_err("total").code(),
            "E_COMPAT_INVALID"
        );

        let mut plan = fo_plan(&temp, b"mrpack-bytes");
        plan.overrides = vec!["mods/../../user.jar".to_string()];
        let path = write_plan_file(&temp, &plan);
        assert_eq!(
            load_plan(&path, &temp).expect_err("override path").code(),
            "E_COMPAT_INVALID"
        );
    }

    #[test]
    fn mods_guard_accepts_missing_or_empty_directory() {
        let temp = TempDir::new("compat-mods-empty");
        let game = TempDir::new("compat-game-empty");
        let plan = fo_plan(&temp, b"mrpack-bytes");

        assert!(mods_owned_by_plan(&plan, &game));
        std::fs::create_dir(game.join("mods")).expect("mods");
        assert!(mods_owned_by_plan(&plan, &game));
    }

    #[test]
    fn mods_guard_accepts_only_files_owned_by_plan() {
        let temp = TempDir::new("compat-mods-owned");
        let game = TempDir::new("compat-game-owned");
        let mut plan = fo_plan(&temp, b"mrpack-bytes");
        plan.overrides = vec!["mods/options/fo.txt".to_string()];

        std::fs::create_dir_all(game.join("mods/options")).expect("mods");
        std::fs::write(game.join("mods/sodium.jar"), b"jar").expect("index mod");
        std::fs::write(game.join("mods/options/fo.txt"), b"override").expect("override");

        assert!(mods_owned_by_plan(&plan, &game));
    }

    #[test]
    fn mods_guard_rejects_foreign_file() {
        let temp = TempDir::new("compat-mods-foreign");
        let game = TempDir::new("compat-game-foreign");
        let plan = fo_plan(&temp, b"mrpack-bytes");

        std::fs::create_dir(game.join("mods")).expect("mods");
        std::fs::write(game.join("mods/sodium.jar"), b"jar").expect("owned");
        std::fs::write(game.join("mods/user-mod.jar"), b"user").expect("foreign");

        assert!(!mods_owned_by_plan(&plan, &game));
    }

    #[test]
    fn plan_load_accepts_optifine_and_rejects_bad_forge() {
        let temp = TempDir::new("compat-plan-of");
        let mut plan = CompatPlan {
            schema: COMPAT_PLAN_SCHEMA,
            kind: CompatKind::OptifineForge,
            mc_version: "1.12.2".to_string(),
            display_name: OPTIFINE_DISPLAY.to_string(),
            version: "OptiFine_1.12.2_HD_U_G5.jar".to_string(),
            loader: LoaderKind::Forge,
            loader_version: "14.23.5.2847".to_string(),
            total_bytes: None,
            mrpack_path: None,
            pack_sha512: None,
            pack_size: None,
            files: Vec::new(),
            overrides: Vec::new(),
            protected_paths: Vec::new(),
            optifine: Some(OptifinePlan {
                file_name: "OptiFine_1.12.2_HD_U_G5.jar".to_string(),
                forge_version: "14.23.5.2847".to_string(),
            }),
            note: None,
        };
        let path = write_plan_file(&temp, &plan);
        assert!(load_plan(&path, &temp).is_ok());

        plan.optifine = Some(OptifinePlan {
            file_name: "OptiFine_1.12.2_HD_U_G5.jar".to_string(),
            forge_version: "14.23.5<script>".to_string(),
        });
        let path = write_plan_file(&temp, &plan);
        assert_eq!(
            load_plan(&path, &temp).expect_err("forge").code(),
            "E_COMPAT_INVALID"
        );
    }

    #[tokio::test]
    async fn apply_reports_failure_without_touching_target() {
        let parent = TempDir::new("compat-apply-fail");
        let temp = crate::test_utils::compat_root(&parent);
        let game = TempDir::new("compat-game-fail");
        let summary = CompatSummary {
            kind: CompatKind::OptifineForge,
            display_name: OPTIFINE_DISPLAY.to_string(),
            version: "OptiFine_1.12.2_HD_U_G5.jar".to_string(),
            loader: LoaderKind::Forge,
            loader_version: "14.23.5.2847".to_string(),
            mc_version: "1.12.2".to_string(),
            total_bytes: None,
            files_count: 0,
            apply_bytes: None,
            plan_path: temp.join("compat-plan-missing.json").to_string_lossy().into_owned(),
            note: None,
        };
        let target = crate::domain::GameTarget {
            launcher: crate::domain::LauncherKind::Prism,
            name: "KraiMaps".to_string(),
            path: game.as_ref().to_path_buf(),
            minecraft_version: "1.12.2".to_string(),
            loader: LoaderKind::Forge,
            loader_version: Some("14.23.5.2847".to_string()),
        };
        provenance::register(&target);
        let report = apply_compat(
            &target,
            &summary,
            Path::new(&summary.plan_path),
            &temp,
            Arc::new(|_| {}),
        )
        .await;
        assert_eq!(report.kind, Some(CompatKind::OptifineForge));
        let error = report.error.expect("error");
        assert_eq!(error.code, "E_COMPAT_INVALID");
        assert_eq!(std::fs::read_dir(&game).expect("game").count(), 0);
    }

    #[tokio::test]
    async fn apply_refuses_target_not_created_by_installer() {
        let parent = TempDir::new("compat-provenance");
        let temp = crate::test_utils::compat_root(&parent);
        let game = TempDir::new("compat-provenance-game");
        let summary = CompatSummary {
            kind: CompatKind::FabulouslyOptimized,
            display_name: FO_DISPLAY.to_string(),
            version: "5.4.1".to_string(),
            loader: LoaderKind::Fabric,
            loader_version: "0.15.11".to_string(),
            mc_version: "1.20.1".to_string(),
            total_bytes: Some(100),
            files_count: 1,
            apply_bytes: Some(100),
            plan_path: temp.join("compat-plan-missing.json").to_string_lossy().into_owned(),
            note: None,
        };
        let target = crate::domain::GameTarget {
            launcher: crate::domain::LauncherKind::Prism,
            name: "Чужой инстанс".to_string(),
            path: game.as_ref().to_path_buf(),
            minecraft_version: "1.20.1".to_string(),
            loader: LoaderKind::Fabric,
            loader_version: Some("0.15.11".to_string()),
        };
        let report = apply_compat(
            &target,
            &summary,
            Path::new(&summary.plan_path),
            &temp,
            Arc::new(|_| {}),
        )
        .await;
        let error = report.error.expect("error");
        assert_eq!(error.code, "E_COMPAT_APPLY");
        assert!(
            error.message.contains("созданную установщиком"),
            "сообщение: {}",
            error.message
        );
    }

    #[test]
    fn read_pack_rejects_combined_index_and_override_size_over_two_gibibytes() {
        let temp = TempDir::new("compat-combined-limit");
        let pack = temp.join("pack.mrpack");
        let files: Vec<_> = (0..8).map(|n| serde_json::json!({
            "path": format!("mods/{n}.jar"),
            "hashes": { "sha512": "a".repeat(128) },
            "downloads": [format!("https://cdn.modrinth.com/{n}.jar")],
            "fileSize": 268_435_456,
        })).collect();
        let index = serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "files": files,
            "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.14.23" },
        })).expect("index");
        crate::test_utils::write_zip(&pack, &[
            ("modrinth.index.json", &index),
            ("overrides/options.txt", b"x"),
        ]);

        let result = read_and_parse(&std::fs::read(&pack).expect("pack"), "1.20.1");

        assert_eq!(result.expect_err("combined layer exceeds 2 GiB").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn plan_load_rechecks_combined_index_and_override_budget() {
        let temp = TempDir::new("compat-plan-combined-limit");
        let mut plan = fo_plan(&temp, b"marker");
        plan.files = (0..8).map(|n| file_entry(
            &format!("mods/{n}.jar"),
            &format!("https://cdn.modrinth.com/{n}.jar"),
            268_435_456,
        )).collect();
        plan.total_bytes = Some(2_147_483_648);
        plan.overrides = vec!["options.txt".to_string()];
        let pack = Path::new(plan.mrpack_path.as_deref().expect("pack"));
        crate::test_utils::write_zip(pack, &[
            ("modrinth.index.json", &fo_index(&plan.files)),
            ("overrides/options.txt", b"x"),
        ]);
        plan.pack_sha512 = Some(http::sha512_file(pack).expect("sha"));
        plan.pack_size = Some(std::fs::metadata(pack).expect("pack metadata").len());
        let path = write_plan_file(&temp, &plan);

        assert_eq!(load_plan(&path, &temp).expect_err("combined limit").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn plan_load_rejects_a_pack_larger_than_the_download_limit() {
        use std::io::{Seek, SeekFrom};
        let temp = TempDir::new("compat-pack-size-limit");
        let mut plan = fo_plan(&temp, b"marker");
        let pack = Path::new(plan.mrpack_path.as_deref().expect("pack"));
        let original = std::fs::read(pack).expect("pack bytes");
        let mut oversized = std::fs::File::create(pack).expect("pack");
        oversized.set_len(modrinth_pack::MAX_MRPACK_BYTES).expect("padding");
        oversized.seek(SeekFrom::End(0)).expect("end");
        oversized.write_all(&original).expect("zip");
        drop(oversized);
        plan.pack_size = Some(std::fs::metadata(pack).expect("metadata").len());
        plan.pack_sha512 = Some(http::sha512_file(pack).expect("hash"));
        let path = write_plan_file(&temp, &plan);

        assert_eq!(load_plan(&path, &temp).expect_err("oversized pack").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn plan_load_rejects_files_that_no_longer_match_the_verified_index() {
        let temp = TempDir::new("compat-index-plan-mismatch");
        let mut plan = fo_plan(&temp, b"marker");
        plan.files[0].path = "mods/not-from-this-pack.jar".to_string();
        let path = write_plan_file(&temp, &plan);

        assert_eq!(load_plan(&path, &temp).expect_err("index mismatch").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn plan_load_rejects_duplicate_file_destinations() {
        let temp = TempDir::new("compat-plan-duplicate");
        let mut plan = fo_plan(&temp, b"marker");
        plan.files.push(plan.files[0].clone());
        plan.total_bytes = Some(200);
        let path = write_plan_file(&temp, &plan);

        assert_eq!(load_plan(&path, &temp).expect_err("duplicate path").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn staging_apply_refuses_linked_source_directories() {
        let outside = TempDir::new("compat-source-link-outside");
        let game = TempDir::new("compat-source-link-game");
        let stage = TempDir::new("compat-source-link-stage");
        let plan_root = TempDir::new("compat-source-link-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        std::fs::write(outside.join("a.txt"), b"user").expect("outside file");
        crate::test_utils::link_directory(&outside, &stage.join("config"));

        let result = apply_staging(&stage, &game, &plan, &|_| {});

        assert!(result.is_err(), "linked staging entries must not be silently accepted");
        assert_eq!(std::fs::read_dir(&game).expect("game").count(), 0);
    }

    #[test]
    fn mods_guard_refuses_a_link_even_when_its_files_match_the_plan() {
        let outside = TempDir::new("compat-mods-link-outside");
        let game = TempDir::new("compat-mods-link-game");
        let plan_root = TempDir::new("compat-mods-link-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        std::fs::write(outside.join("sodium.jar"), b"user").expect("outside mod");
        crate::test_utils::link_directory(&outside, &game.join("mods"));

        assert!(!mods_owned_by_plan(&plan, &game));
    }

    #[test]
    fn staging_apply_refuses_directory_links_without_writing_outside_target() {
        let outside = TempDir::new("compat-link-outside");
        let game = TempDir::new("compat-link-game");
        let stage = TempDir::new("compat-link-stage");
        let plan_root = TempDir::new("compat-link-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        std::fs::write(outside.join("a.txt"), b"user").expect("user file");
        crate::test_utils::link_directory(&outside, &game.join("config"));
        std::fs::create_dir(stage.join("config")).expect("stage config");
        std::fs::write(stage.join("config/a.txt"), b"layer").expect("layer file");

        let result = apply_staging(&stage, &game, &plan, &|_| {});

        assert!(result.is_err(), "directory link must be refused");
        assert_eq!(std::fs::read(outside.join("a.txt")).expect("user"), b"user");
    }

    #[test]
    fn staging_apply_refuses_backup_directory_links_before_replacing_file() {
        let outside = TempDir::new("compat-backup-link-outside");
        let game = TempDir::new("compat-backup-link-game");
        let stage = TempDir::new("compat-backup-link-stage");
        let plan_root = TempDir::new("compat-backup-link-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        crate::test_utils::link_directory(&outside, &game.join(".kraimaps-backup"));
        std::fs::write(game.join("options.txt"), b"user").expect("user file");
        std::fs::write(stage.join("options.txt"), b"layer").expect("layer file");

        let result = apply_staging(&stage, &game, &plan, &|_| {});

        assert!(result.is_err(), "backup link must be refused");
        assert_eq!(std::fs::read(game.join("options.txt")).expect("user"), b"user");
        assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
    }

    #[test]
    fn staging_apply_does_not_modify_an_external_hard_link() {
        let outside = TempDir::new("compat-hard-link-outside");
        let game = TempDir::new("compat-hard-link-game");
        let stage = TempDir::new("compat-hard-link-stage");
        let plan_root = TempDir::new("compat-hard-link-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        std::fs::write(outside.join("a.txt"), b"user").expect("user file");
        std::fs::hard_link(outside.join("a.txt"), game.join("options.txt")).expect("hard link");
        std::fs::write(stage.join("options.txt"), b"layer").expect("layer file");

        apply_staging(&stage, &game, &plan, &|_| {}).expect("apply");

        assert_eq!(std::fs::read(outside.join("a.txt")).expect("user"), b"user");
        assert_eq!(std::fs::read(game.join("options.txt")).expect("layer"), b"layer");
    }

    #[test]
    fn staging_apply_backs_up_replaced_files() {
        let game = TempDir::new("compat-apply-game");
        let stage = TempDir::new("compat-apply-stage");
        std::fs::create_dir_all(stage.join("config")).expect("config");
        std::fs::create_dir_all(stage.join("mods")).expect("mods");
        std::fs::write(stage.join("config/a.txt"), b"new").expect("a");
        std::fs::write(stage.join("mods/b.jar"), b"jar").expect("b");

        std::fs::create_dir_all(game.join("config")).expect("game config");
        std::fs::write(game.join("config/a.txt"), b"old").expect("old");

        let last = AtomicU64::new(0);
        let plan_root = TempDir::new("compat-apply-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        let report = apply_staging(&stage, &game, &plan, &|ev| {
            assert_eq!(ev.stage, "compat-apply");
            last.store(ev.done_bytes, Ordering::Relaxed);
        })
        .expect("apply");
        assert_eq!(report, 2);
        assert_eq!(std::fs::read(game.join("config/a.txt")).expect("a"), b"new");
        assert_eq!(std::fs::read(game.join("mods/b.jar")).expect("b"), b"jar");
        assert_eq!(last.load(Ordering::Relaxed), 6, "new + jar");

        let stamp = std::fs::read_dir(game.join(".kraimaps-backup"))
            .expect("root")
            .next()
            .expect("stamp")
            .expect("entry")
            .path();
        assert_eq!(
            std::fs::read(stamp.join("config/a.txt")).expect("backup"),
            b"old"
        );
        assert!(!stamp.join("mods/b.jar").exists(), "новый файл не бэкапится");
    }

    #[test]
    fn protected_resourcepack_directories_preserve_every_map_file_on_retry() {
        let game = TempDir::new("compat-rp-priority-game");
        let stage = TempDir::new("compat-rp-priority-stage");
        let plan_root = TempDir::new("compat-rp-priority-plan");
        let mut plan = fo_plan(&plan_root, b"marker");
        plan.protected_paths = vec!["resourcepacks/Story_v2".to_string()];
        std::fs::create_dir_all(game.join("resourcepacks/Story_v2/assets/map")).expect("map pack");
        std::fs::create_dir_all(stage.join("resourcepacks/Story_v2/assets/map")).expect("layer pack");
        std::fs::write(game.join("resourcepacks/Story_v2/assets/map/story.json"), b"map").expect("map");
        std::fs::write(stage.join("resourcepacks/Story_v2/assets/map/story.json"), b"layer").expect("layer");

        apply_staging(&stage, &game, &plan, &|_| {}).expect("apply");

        assert_eq!(std::fs::read(game.join("resourcepacks/Story_v2/assets/map/story.json")).expect("map"), b"map");
    }

    #[cfg(windows)]
    #[test]
    fn protected_paths_use_windows_case_insensitive_comparison() {
        let game = TempDir::new("compat-case-priority-game");
        let stage = TempDir::new("compat-case-priority-stage");
        let plan_root = TempDir::new("compat-case-priority-plan");
        let mut plan = fo_plan(&plan_root, b"marker");
        plan.protected_paths = vec!["Config/Map.TXT".to_string()];
        std::fs::create_dir_all(game.join("config")).expect("game config");
        std::fs::create_dir_all(stage.join("config")).expect("stage config");
        std::fs::write(game.join("config/map.txt"), b"map").expect("map");
        std::fs::write(stage.join("config/map.txt"), b"layer").expect("layer");

        apply_staging(&stage, &game, &plan, &|_| {}).expect("apply");

        assert_eq!(std::fs::read(game.join("config/map.txt")).expect("map"), b"map");
    }

    #[test]
    fn protected_paths_do_not_recreate_missing_map_files() {
        let game = TempDir::new("compat-missing-map-file-game");
        let stage = TempDir::new("compat-missing-map-file-stage");
        let plan_root = TempDir::new("compat-missing-map-file-plan");
        let mut plan = fo_plan(&plan_root, b"marker");
        plan.protected_paths = vec!["config/map.txt".to_string()];
        std::fs::create_dir_all(stage.join("config")).expect("stage config");
        std::fs::write(stage.join("config/map.txt"), b"layer").expect("layer");

        let written = apply_staging(&stage, &game, &plan, &|_| {}).expect("apply");

        assert_eq!(written, 0);
        assert!(!game.join("config/map.txt").exists());
    }

    #[test]
    fn compat_apply_keeps_map_content_over_layer_overrides() {
        let game = TempDir::new("compat-priority-game");
        let stage = TempDir::new("compat-priority-stage");
        let plan_root = TempDir::new("compat-priority-plan");
        let mut plan = fo_plan(&plan_root, b"mrpack-bytes");
        plan.protected_paths = vec!["config/map.txt".to_string()];

        std::fs::create_dir_all(stage.join("config")).expect("stage config");
        std::fs::write(stage.join("config/map.txt"), b"layer").expect("layer map");
        std::fs::write(stage.join("config/other.txt"), b"layer").expect("layer other");

        std::fs::create_dir_all(game.join("config")).expect("game config");
        std::fs::write(game.join("config/map.txt"), b"map").expect("map");
        std::fs::write(game.join("config/other.txt"), b"old").expect("old");

        apply_staging(&stage, &game, &plan, &|_| {}).expect("apply");

        assert_eq!(
            std::fs::read(game.join("config/map.txt")).expect("map"),
            b"map",
            "контент карты главнее overrides сборки"
        );
        assert_eq!(
            std::fs::read(game.join("config/other.txt")).expect("other"),
            b"layer",
            "файл вне контента карты обновляется слоем"
        );
    }

    #[tokio::test]
    async fn validated_optifine_retry_preserves_world_and_resourcepack_conflict_names() {
        use crate::domain::{LauncherKind, ResourcePackKind, ResourcePackSelection};
        use crate::test_utils::{compat_root, wait_for_entry_count, write_stored_zip, write_zip};
        use sha2::Digest;

        let source = TempDir::new("compat-optifine-retry-source");
        let game = TempDir::new("compat-optifine-retry-game");
        let temp = compat_root(&source);
        let plan = optifine_plan();
        let plan_path = write_plan_file(&temp, &plan);
        let target = GameTarget {
            launcher: LauncherKind::Official, name: "temporary OptiFine target".into(),
            path: game.to_path_buf(), minecraft_version: "1.12.2".into(),
            loader: LoaderKind::Forge, loader_version: Some("14.23.5.2847".into()),
        };
        provenance::register(&target);
        let map_path = source.join("map.zip");
        write_zip(&map_path, &[("W/level.dat", b"world"), ("config/map.cfg", b"map"),
            ("Story/pack.mcmeta", b"map pack")]);
        std::fs::create_dir_all(game.join("saves/W")).expect("existing world");
        std::fs::write(game.join("saves/W/level.dat"), b"user world").expect("existing world data");
        std::fs::create_dir_all(game.join("resourcepacks/Story")).expect("existing pack");
        std::fs::write(game.join("resourcepacks/Story/pack.mcmeta"), b"user pack").expect("existing pack data");
        let world_name = deploy::deploy_world(&map_path, "W/", "W", &game.join("saves"), &|_| {})
            .expect("install map before compatibility");
        let map_plan = deploy::DeployPlan {
            zip_path: map_path.clone(), world_prefix: "W/".into(), world_name: world_name.clone(),
            content_prefix: String::new(), overrides_alias: false, layers: Vec::new(),
            deploy_mods: false, wipe_mods: false, compat_plan_path: None,
            resource_packs: vec![ResourcePackSelection {
                source: "Story/".into(), name: "Story".into(), kind: ResourcePackKind::Folder,
            }],
        };
        protect_target_content_paths(&target, &plan_path, &temp,
            deploy::content_paths(&map_plan, &world_name).expect("map destinations"))
            .expect("map protection");
        let packs = deploy::deploy_resourcepacks(&map_path, &map_plan.resource_packs,
            &game.join("resourcepacks"), &|_| {}, &mut |name| {
                protect_target_content_paths(&target, &plan_path, &temp,
                    vec![format!("resourcepacks/{name}")]).expect("final resourcepack protection");
            }).expect("map resourcepacks");
        deploy::deploy_layers(&map_path, "W/", "", false, &game, &world_name,
            &["Story/".into()], &[], &|_| {}).expect("map content");
        assert_eq!(world_name, "W_v2");
        assert_eq!(packs, vec!["Story_v2"]);

        let jar_name = plan.optifine.as_ref().expect("OptiFine plan").file_name.as_str();
        let jar_path = source.join(jar_name);
        write_stored_zip(&jar_path, &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\nTweakClass: optifine.OptiFineForgeTweaker\r\n"),
            ("optifine/Installer.class", b"\xca\xfe\xba\xbe"),
            ("padding.bin", &vec![0; optifine::MIN_JAR_BYTES as usize]),
        ]);
        let jar_bytes = std::fs::read(&jar_path).expect("jar fixture");
        let expected_sha = http::hex(sha2::Sha256::digest(&jar_bytes));
        let bound = bind_plan_to_target(load_plan(&plan_path, &temp).expect("retained plan"), &target)
            .expect("bind owned target");
        let obstruction = game.join("mods").join(jar_name);
        std::fs::create_dir_all(&obstruction).expect("write obstruction");
        {
            let stage = temp.staging().expect("first staging");
            std::fs::create_dir(stage.join("mods")).expect("stage mods");
            let staged_jar = stage.join("mods").join(jar_name);
            std::fs::copy(&jar_path, &staged_jar).expect("stage jar");
            assert_eq!(optifine::validate_jar(&staged_jar, jar_name).expect("validated jar"), expected_sha);
            assert!(mods_owned_by_plan(&bound, &game));

            assert_eq!(apply_staging(&stage, &game, &bound, &|_| {})
                .expect_err("blocked OptiFine write must fail").code(), "E_COMPAT_APPLY");
        }
        wait_for_entry_count(&temp, 2).await;
        assert_eq!(std::fs::read(game.join("saves/W_v2/level.dat")).expect("installed world"), b"world");
        std::fs::remove_dir(&obstruction).expect("remove fixture obstruction");
        std::fs::write(game.join("saves/W_v2/level.dat"), b"player progress").expect("played world");
        std::fs::remove_file(map_path).expect("map archive not needed for compatibility retry");
        {
            let stage = temp.staging().expect("retry staging");
            std::fs::create_dir(stage.join("mods")).expect("retry mods");
            let staged_jar = stage.join("mods").join(jar_name);
            std::fs::copy(&jar_path, &staged_jar).expect("restage jar");
            assert_eq!(optifine::validate_jar(&staged_jar, jar_name).expect("revalidated jar"), expected_sha);

            assert_eq!(apply_staging(&stage, &game, &bound, &|_| {}).expect("retry OptiFine"), 1);
        }

        assert_eq!(std::fs::read(&obstruction).expect("installed OptiFine"), jar_bytes);
        assert!(mods_owned_by_plan(&bound, &game), "our jar must remain retryable");
        assert_eq!(std::fs::read(game.join("saves/W/level.dat")).expect("user world"), b"user world");
        assert_eq!(std::fs::read(game.join("saves/W_v2/level.dat")).expect("played world"), b"player progress");
        assert_eq!(std::fs::read(game.join("resourcepacks/Story/pack.mcmeta")).expect("user pack"), b"user pack");
        assert_eq!(std::fs::read(game.join("resourcepacks/Story_v2/pack.mcmeta")).expect("map pack"), b"map pack");
        assert_eq!(std::fs::read(game.join("config/map.cfg")).expect("map content"), b"map");
        wait_for_entry_count(&temp, 2).await;
    }

    #[tokio::test]
    async fn layer_deadline_prevents_committing_a_file_after_synchronous_copy() {
        let temp = TempDir::new("compat-local-deadline");
        let game = TempDir::new("compat-local-deadline-game");
        let mut plan = fo_plan(&temp, b"marker");
        plan.files.clear();
        plan.total_bytes = Some(0);
        let pack_path = temp.join("overrides.mrpack");
        crate::test_utils::write_zip(&pack_path, &[("overrides/options.txt", b"layer")]);
        let pack = std::fs::read(pack_path).expect("pack");
        std::fs::write(game.join("options.txt"), b"user").expect("original");

        let result = apply_plan_within(&game, &plan, Some(&pack), &temp, Arc::new(|event| {
            if event.stage == "compat-apply" && event.done_bytes > 0 {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }), std::time::Duration::from_millis(100)).await;

        assert!(result.is_err(), "local file work must respect the layer deadline");
        assert_eq!(std::fs::read(game.join("options.txt")).expect("original"), b"user");
        assert_eq!(std::fs::read_dir(&temp).expect("temp").count(), 2, "staging must be removed");
        assert!(!std::fs::read_dir(&game).expect("game").any(|entry| {
            entry.expect("entry").file_name().to_string_lossy().starts_with(".compat-write-")
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_local_apply_prevents_later_commits_and_removes_staging() {
        struct Completion(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for Completion {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let temp = Arc::new(TempDir::new("compat-cancel-local"));
        let game = Arc::new(TempDir::new("compat-cancel-local-game"));
        let mut plan = fo_plan(&temp, b"marker");
        plan.files.clear();
        plan.total_bytes = Some(0);
        let pack_path = temp.join("overrides.mrpack");
        crate::test_utils::write_zip(&pack_path, &[("overrides/options.txt", b"layer")]);
        let pack = std::fs::read(pack_path).expect("pack");
        std::fs::write(game.join("options.txt"), b"user").expect("original");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started = std::sync::Mutex::new(Some(started_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = std::sync::Mutex::new(release_rx);
        let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
        let completion = Completion(Some(finished_tx));
        let progress = move |event: deploy::StageEvent| {
            let _ = &completion;
            if event.stage == "compat-apply" && event.done_bytes > 0 {
                if let Some(sender) = started.lock().expect("started").take() {
                    let _ = sender.send(());
                    release.lock().expect("release").recv().expect("release worker");
                }
            }
        };
        let (task_temp, task_game) = (temp.clone(), game.clone());
        let mut attempt = tokio::spawn(async move {
            apply_plan_within(&task_game, &plan, Some(&pack), &task_temp, Arc::new(progress),
                std::time::Duration::from_secs(30)).await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), started_rx).await
            .expect("worker must start").expect("started");
        attempt.abort();
        let cancelled = tokio::time::timeout(std::time::Duration::from_millis(200), &mut attempt).await;
        release_tx.send(()).expect("release worker");
        if cancelled.is_err() {
            let _ = attempt.await;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), finished_rx).await
            .expect("worker must stop").expect("completion");

        assert!(cancelled.is_ok(), "cancellation must not block behind synchronous file work");
        assert_eq!(std::fs::read(game.join("options.txt")).expect("original"), b"user");
        assert_eq!(std::fs::read_dir(&*temp).expect("temp").count(), 2, "worker owns staging until it stops");
    }

    #[tokio::test]
    async fn cancelling_an_apply_future_removes_its_staging() {
        let temp = TempDir::new("compat-cancel-stage");
        let game = TempDir::new("compat-cancel-game");
        let plan_root = TempDir::new("compat-cancel-plan");
        let plan = fo_plan(&plan_root, b"marker");
        let pack = std::fs::read(plan.mrpack_path.as_deref().expect("pack")).expect("pack bytes");
        let mut attempt = Box::pin(apply_plan_within(&game, &plan, Some(&pack), &temp, Arc::new(|_| {}), std::time::Duration::from_secs(30)));
        assert!(futures_util::poll!(&mut attempt).is_pending());
        assert_eq!(std::fs::read_dir(&temp).expect("staging parent").count(), 1);

        drop(attempt);

        crate::test_utils::wait_for_entry_count(&temp, 0).await;
        assert_eq!(std::fs::read_dir(&game).expect("game").count(), 0);
    }

    #[tokio::test]
    async fn apply_gives_up_when_layer_deadline_expires() {
        let temp = TempDir::new("compat-deadline");
        let game = TempDir::new("compat-deadline-game");
        let plan_root = TempDir::new("compat-deadline-plan");
        let plan = fo_plan(&plan_root, b"mrpack-bytes");
        let pack = std::fs::read(plan.mrpack_path.as_deref().expect("pack")).expect("pack bytes");
        let err = apply_plan_within(
            &game,
            &plan,
            Some(&pack),
            &temp,
            Arc::new(|_| {}),
            std::time::Duration::from_millis(1),
        )
        .await
        .expect_err("deadline");
        assert_eq!(err.code(), "E_COMPAT_APPLY");
        assert!(
            err.details().contains("не успела"),
            "детали: {}",
            err.details()
        );
    }

    #[test]
    fn discard_refuses_foreign_plan_before_reading_its_pack_path() {
        let temp = TempDir::new("compat-discard-owned");
        let outside = TempDir::new("compat-discard-outside");
        let plan = fo_plan(&temp, b"marker");
        let foreign_plan = write_plan_file(&outside, &plan);
        let pack = Path::new(plan.mrpack_path.as_deref().expect("pack"));

        assert_eq!(discard_compat(&foreign_plan, &temp), 0);
        assert!(foreign_plan.exists());
        assert!(pack.exists(), "foreign plan must not delete another flow's pack");
    }

    #[test]
    fn layer_apply_keeps_map_content_after_a_protection_save_failure() {
        let temp = TempDir::new("compat-failed-protection-save");
        let game = TempDir::new("compat-failed-protection-game");
        let stage = TempDir::new("compat-failed-protection-stage");
        let plan = fo_plan(&temp, b"marker");
        let target = GameTarget {
            launcher: crate::domain::LauncherKind::Official,
            path: game.to_path_buf(),
            name: "Story".into(),
            minecraft_version: plan.mc_version.clone(),
            loader: plan.loader,
            loader_version: Some(plan.loader_version.clone()),
        };
        provenance::register(&target);
        std::fs::create_dir_all(game.join("resourcepacks/Story_v2")).expect("map pack");
        std::fs::create_dir_all(stage.join("resourcepacks/Story_v2")).expect("layer pack");
        std::fs::write(game.join("resourcepacks/Story_v2/pack.mcmeta"), b"map").expect("map");
        std::fs::write(stage.join("resourcepacks/Story_v2/pack.mcmeta"), b"layer").expect("layer");
        let path = temp.join("compat-plan-missing.json");

        assert!(protect_target_content_paths(&target, &path, &temp,
            vec!["resourcepacks/Story_v2".into(), " readme.txt".into()]).is_err());
        let bound = bind_plan_to_target(plan, &target).expect("bound plan");
        apply_staging(&stage, &game, &bound, &|_| {}).expect("retry");

        assert_eq!(std::fs::read(game.join("resourcepacks/Story_v2/pack.mcmeta")).expect("map"), b"map");
    }

    #[test]
    fn invalid_map_protection_cannot_authorize_a_compatibility_retry() {
        let temp = TempDir::new("compat-invalid-protection");
        let game = TempDir::new("compat-invalid-protection-game");
        let plan = optifine_plan();
        let path = write_plan_file(&temp, &plan);
        let target = GameTarget {
            launcher: crate::domain::LauncherKind::Official,
            path: game.to_path_buf(), name: "Invalid protection".into(),
            minecraft_version: plan.mc_version.clone(), loader: plan.loader,
            loader_version: Some(plan.loader_version.clone()),
        };
        provenance::register(&target);

        assert!(protect_target_content_paths(&target, &path, &temp,
            vec!["config/map.cfg".into(), "../outside".into()]).is_err());

        assert_eq!(bind_plan_to_target(plan, &target).expect_err("incomplete protection must deny retry").code(),
            "E_COMPAT_INVALID");
    }

    #[cfg(windows)]
    #[test]
    fn compatibility_retry_protects_actual_windows_destinations_after_plan_save_failure() {
        let temp = TempDir::new("compat-windows-protection");
        let game = TempDir::new("compat-windows-protection-game");
        let stage = TempDir::new("compat-windows-protection-stage");
        let plan = fo_plan(&temp, b"marker");
        std::fs::create_dir(game.join("config")).expect("map config");
        std::fs::write(game.join("config/map.cfg"), b"map").expect("map file");
        std::fs::create_dir(stage.join("config")).expect("layer config");
        std::fs::write(stage.join("config/map.cfg"), b"layer").expect("layer file");
        for (index, raw) in ["config/map.cfg ", "config./map.cfg", "config/./map.cfg",
            "config//map.cfg", "config/subdir/../map.cfg"].into_iter().enumerate() {
            let target = GameTarget {
                launcher: crate::domain::LauncherKind::Official,
                path: game.to_path_buf(), name: format!("Windows protection {index}"),
                minecraft_version: plan.mc_version.clone(), loader: plan.loader,
                loader_version: Some(plan.loader_version.clone()),
            };
            provenance::register(&target);
            assert!(protect_target_content_paths(&target, &temp.join("compat-plan-missing.json"), &temp,
                vec![raw.into()]).is_err());

            let bound = bind_plan_to_target(plan.clone(), &target).expect("bound plan");
            apply_staging(&stage, &game, &bound, &|_| {}).expect("retry");

            assert_eq!(std::fs::read(game.join("config/map.cfg")).expect("map"), b"map", "{raw}");
        }
    }

    #[test]
    fn plan_protection_accumulates_final_resourcepack_destinations() {
        let temp = TempDir::new("compat-protection-union");
        let plan = fo_plan(&temp, b"marker");
        let path = write_plan_file(&temp, &plan);
        protect_content_paths(&path, &temp, vec!["config/map.txt".into()]).expect("base protection");

        protect_content_paths(&path, &temp, vec!["resourcepacks/Story_v2".into()]).expect("final pack protection");

        assert_eq!(load_plan(&path, &temp).expect("plan").protected_paths,
            vec!["config/map.txt", "resourcepacks/Story_v2"]);
    }

    #[test]
    fn plan_save_does_not_modify_a_hard_link_at_the_old_part_path() {
        let temp = TempDir::new("compat-save-part");
        let outside = TempDir::new("compat-save-part-outside");
        let plan = fo_plan(&temp, b"marker");
        let path = write_plan_file(&temp, &plan);
        let user_file = outside.join("notes.txt");
        std::fs::write(&user_file, b"user").expect("user file");
        std::fs::hard_link(&user_file, path.with_extension("json.part")).expect("hard link");

        protect_content_paths(&path, &temp, vec!["config/map.txt".to_string()]).expect("save");

        assert_eq!(std::fs::read(&user_file).expect("user"), b"user");
        assert_eq!(load_plan(&path, &temp).expect("plan").protected_paths, vec!["config/map.txt"]);
    }

    #[test]
    fn plan_load_rejects_a_valid_json_document_over_the_size_limit() {
        let temp = TempDir::new("compat-large-plan");
        let plan = fo_plan(&temp, b"marker");
        let path = write_plan_file(&temp, &plan);
        let mut raw = serde_json::to_vec(&plan).expect("plan");
        raw.resize(8 * 1024 * 1024 + 1, b' ');
        std::fs::write(&path, raw).expect("large plan");

        assert_eq!(load_plan(&path, &temp).expect_err("large plan").code(), "E_COMPAT_INVALID");
    }

    #[test]
    fn discard_removes_plan_and_pack_only() {
        let temp = TempDir::new("compat-discard");
        let plan = fo_plan(&temp, b"mrpack-bytes");
        let path = write_plan_file(&temp, &plan);
        let pack = PathBuf::from(plan.mrpack_path.as_deref().expect("pack"));
        let foreign = temp.join("notes.txt");
        std::fs::write(&foreign, b"x").expect("foreign");

        assert_eq!(discard_compat(&path, &temp), 2);
        assert!(!path.exists());
        assert!(!pack.exists());
        assert!(foreign.exists(), "чужие файлы не трогаем");
    }
}
