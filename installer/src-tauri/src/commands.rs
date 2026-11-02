//! Tauri-команды: тонкий мост между движком (`services`/`adapters`)
//! и фронтендом. Никакой логики здесь нет — только сериализация,
//! `spawn_blocking` для тяжёлых файловых операций и проброс прогресса
//! в событие `deploy-progress`. Ошибки — объектом
//! `{code, message, details}` (код `E_*` для «Подробнее»).

use crate::adapters::{legacy, modrinth, official, prism, tlauncher, vanilla};
use crate::domain::{
    CompatFailure, CompatReport, CompatSummary, GameTarget, InstallError, LauncherKind, LoaderKind,
    PackAnalysis,
};
use crate::services::{
    auth, compat, deploy, detect, download, normalize, provenance, site, update_policy, versions,
};
use crate::services::compat_temp::{CompatLease, CompatTemp};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

#[derive(Debug, Serialize)]
pub struct CmdError {
    code: String,
    message: String,
    details: String,
}

impl From<InstallError> for CmdError {
    fn from(e: InstallError) -> Self {
        Self {
            code: e.code().to_string(),
            message: e.to_string(),
            details: e.details(),
        }
    }
}

impl From<tauri::Error> for CmdError {
    fn from(e: tauri::Error) -> Self {
        Self {
            code: "E_IO".to_string(),
            message: "внутренняя ошибка выполнения".to_string(),
            details: format!("join: {e}"),
        }
    }
}

type Cmd<T> = Result<T, CmdError>;

#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

/// Единая версия установщика для фронта (single source: `site::INSTALLER_VERSION`
/// из `Cargo.toml`). Фронт шлёт её в `install-start` как `installer_version`.
#[tauri::command]
pub fn get_installer_version() -> String {
    site::INSTALLER_VERSION.to_string()
}

/// Анализ локального zip: модель пака для экрана подтверждения.
#[tauri::command]
pub async fn analyze_pack(path: String) -> Cmd<PackAnalysis> {
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || normalize::analyze_zip(&path))
        .await?
        .map_err(CmdError::from)
}

/// Детект лаунчеров. `known_roots` — ранее найденные exe/корни (запоминает
/// фронтенд), чтобы портативка находилась и без запуска.
#[tauri::command]
pub async fn detect_launchers(known_roots: Vec<String>) -> Cmd<detect::Detection> {
    let known: Vec<PathBuf> = known_roots.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || detect::detect_launchers(&known))
        .await
        .map_err(CmdError::from)
}

/// Точная версия загрузчика для нового инстанса (через meta-API при нужде).
#[tauri::command]
pub async fn resolve_loader_version(
    loader: LoaderKind,
    mc_version: String,
    hint: Option<String>,
) -> Cmd<Option<String>> {
    versions::resolve_loader_version(loader, &mc_version, hint.as_deref())
        .await
        .map_err(CmdError::from)
}

/// Молчаливое создание инстанса/профиля нужной версии+загрузчика.
#[tauri::command]
pub async fn create_instance(
    launcher: LauncherKind,
    data_dir: String,
    name: String,
    mc_version: String,
    loader: LoaderKind,
    loader_version: Option<String>,
    icon_path: Option<String>,
) -> Cmd<GameTarget> {
    let data_dir = PathBuf::from(data_dir);
    let icon_path = icon_path.map(PathBuf::from);
    let target = tauri::async_runtime::spawn_blocking(move || match launcher {
        LauncherKind::Prism
        | LauncherKind::Pinecone
        | LauncherKind::Polymc
        | LauncherKind::Multimc => prism::create_mmc_instance(
            launcher,
            &data_dir,
            &name,
            &mc_version,
            loader,
            loader_version.as_deref(),
            icon_path.as_deref(),
        ),
        LauncherKind::Modrinth => modrinth::create_modrinth_profile(
            &data_dir,
            &name,
            &mc_version,
            loader,
            loader_version.as_deref(),
            icon_path.as_deref(),
        ),
        // У TLauncher и Legacy Launcher нет инстансов: версии готовятся
        // командами `ensure_tlauncher_version` / `ensure_legacy_version`.
        LauncherKind::Tlauncher | LauncherKind::Legacy => Err(InstallError::NoLauncher),
        // У официального лаунчера своя модель (Установки) — отдельная команда.
        LauncherKind::Official => Err(InstallError::NoLauncher),
    })
    .await??;
    // Инстанс создан здесь — только в него можно ставить слой совместимости.
    provenance::register(&target);
    Ok(target)
}

/// Закрывает запущенный лаунчер на время установки. Возвращает, был ли он
/// запущен (фронт по этому флагу решает, открывать ли его обратно).
/// Никогда не ошибается: не-закрытие — не повод останавливать установку.
#[tauri::command]
pub async fn close_launcher(launcher: LauncherKind) -> bool {
    tauri::async_runtime::spawn_blocking(move || detect::close_launcher(launcher))
        .await
        .unwrap_or(false)
}

/// Открывает лаунчер после установки (только если был запущен на старте —
/// это решает фронт по флагу `close_launcher`). Ошибка (нет exe, не
/// стартует) — в `CmdError`, фронт её глотает молча: установка уже успешна.
///
/// Вид запуска зависит от лаунчера, а не только от exe: у Store-сборки
/// официального лаунчера exe нет вовсе (`WindowsApps` закрыт для обычного
/// приложения), и открывать её можно только по AUMID. Список exe в этом
/// случае приходит пустым, и открытие по файлу падало бы с `E_NO_LAUNCHER`
/// всегда — то есть официальный лаунчер после установки не возвращался.
#[tauri::command]
pub async fn reopen_launcher(launcher: LauncherKind, exe: Option<String>) -> Cmd<String> {
    let exe = exe.map(PathBuf::from);
    tauri::async_runtime::spawn_blocking(move || match launcher {
        LauncherKind::Official => official::launch_official(exe.as_deref()),
        _ => {
            let exe = exe.ok_or(InstallError::NoLauncher)?;
            detect::reopen_launcher(&exe)
        }
    })
    .await?
    .map_err(CmdError::from)
}

/// Прямой деплой модели в цель. Прогресс — событиями `deploy-progress`.
/// `compat` — сводка слоя совместимости; применяется только при
/// `plan.compat_plan_path` и свежей цели.
#[tauri::command]
pub async fn deploy_pack(
    app: tauri::AppHandle,
    target: GameTarget,
    plan: deploy::DeployPlan,
    compat: Option<CompatSummary>,
) -> Cmd<deploy::DeployReport> {
    // Корень игры зависит от лаунчера: у Prism-семейства — папка инстанса,
    // у Modrinth/TLauncher/Legacy/официального — сам game dir. Единая точка
    // правды — `normalize::game_root`.
    let game_root = Arc::new(normalize::game_root(&target));
    let temp_root = if plan.compat_plan_path.is_some() {
        compat_temp_root(&app).map(Some)
    } else {
        Ok(None)
    };
    let compat_bytes = plan
        .compat_plan_path
        .as_ref()
        .and(compat.as_ref().and_then(|c| c.total_bytes));
    let compat_apply_bytes = plan.compat_plan_path.as_ref()
        .and(compat.as_ref().and_then(|summary| summary.apply_bytes));
    let totals = deploy::stage_totals(&plan, compat_bytes, compat_apply_bytes).map_err(CmdError::from)?;
    let overall = Arc::new(Mutex::new(OverallState::new(&totals, &plan)));
    let progress: ProgressFn = {
        let overall = overall.clone();
        Arc::new(move |mut ev: deploy::StageEvent| {
            let mut st = overall.lock().expect("overall progress state");
            if !st.decorate(&mut ev) {
                return;
            }
            // Финал не троттлим: иначе UI может не получить 100% перед Done.
            let finished = ev.done_bytes >= ev.total_bytes;
            let now = Instant::now();
            if finished || now.duration_since(st.last_emit) >= Duration::from_millis(50) {
                st.last_emit = now;
                app.emit("deploy-progress", &ev).ok();
            }
        })
    };
    deploy_into(
        game_root,
        Arc::new(target),
        Arc::new(plan),
        compat,
        temp_root,
        progress,
        overall,
        totals,
    )
    .await
    .map_err(CmdError::from)
}

/// Смещение сквозного прогресса: сколько байт завершённых стадий уже
/// позади (`base`), вес текущей стадии и общий знаменатель.
struct OverallState {
    base: u64,
    stage: &'static str,
    stage_total: u64,
    stage_done: u64,
    total: u64,
    compat_phase: Option<CompatPhase>,
    last_emit: Instant,
}

enum CompatPhase {
    Download { apply_total: u64 },
    Apply,
}

impl OverallState {
    fn new(totals: &deploy::StageTotals, plan: &deploy::DeployPlan) -> Self {
        Self {
            base: 0,
            stage: "world",
            stage_total: totals.world,
            stage_done: 0,
            total: totals.overall(plan.deploy_mods, !plan.resource_packs.is_empty()),
            compat_phase: None,
            last_emit: Instant::now() - Duration::from_secs(1),
        }
    }

    fn decorate(&mut self, ev: &mut deploy::StageEvent) -> bool {
        if self.compat_phase.is_none()
            && matches!(ev.stage.as_str(), "compat-download" | "compat-apply")
        {
            return false;
        }
        if ev.stage == "compat-apply" {
            if let Some(CompatPhase::Download { apply_total }) = self.compat_phase {
                self.base = self.base.saturating_add(self.stage_total);
                self.stage = "compat-apply";
                self.stage_total = apply_total;
                self.stage_done = 0;
                self.compat_phase = Some(CompatPhase::Apply);
            }
        }
        // Отменённый blocking-worker может закончить callback после перехода.
        if ev.stage != self.stage {
            return false;
        }
        if self.compat_phase.is_some() && ev.total_bytes < self.stage_total {
            self.total = self.total.saturating_sub(self.stage_total - ev.total_bytes);
            self.stage_total = ev.total_bytes;
        }
        self.stage_done = self.stage_done.max(ev.done_bytes.min(self.stage_total));
        if self.total > 0 {
            ev.overall_done = Some(self.base.saturating_add(self.stage_done).min(self.total));
            ev.overall_total = Some(self.total);
        }
        true
    }

    fn begin_compat(&mut self, totals: &deploy::StageTotals) {
        self.stage = "compat-download";
        self.stage_total = totals.compat.unwrap_or(0);
        self.stage_done = 0;
        self.compat_phase = Some(CompatPhase::Download { apply_total: totals.compat_apply.unwrap_or(0) });
    }

    fn finish_compat(&mut self) {
        let pending_apply = match self.compat_phase {
            Some(CompatPhase::Download { apply_total }) => apply_total,
            _ => 0,
        };
        let unfinished = self.stage_total.saturating_sub(self.stage_done).saturating_add(pending_apply);
        self.total = self.total.saturating_sub(unfinished);
        self.base = self.base.saturating_add(self.stage_done);
        self.stage_done = 0;
        self.compat_phase = None;
    }
}

/// Переключение обёртки прогресса на следующую стадию.
fn enter_stage(overall: &Mutex<OverallState>, stage: &'static str, stage_total: u64, finished: u64) {
    let mut st = overall.lock().expect("overall progress state");
    st.base = st.base.saturating_add(finished);
    st.stage = stage;
    st.stage_total = stage_total;
    st.stage_done = 0;
}

/// Обёртка прогресса, общая для всех фаз деплоя: файловые фазы выполняются
/// в blocking-пуле, слой совместимости — в async-задаче.
type ProgressFn = deploy::ProgressFn;

/// Сбой blocking-пула (паника задачи или закрытый пул).
fn joined<T>(res: Result<Result<T, InstallError>, tauri::Error>) -> Result<T, InstallError> {
    match res {
        Ok(inner) => inner,
        Err(e) => Err(InstallError::Io(std::io::Error::other(format!(
            "blocking task: {e}"
        )))),
    }
}

#[allow(clippy::too_many_arguments)]
async fn deploy_into(
    game_root: Arc<PathBuf>,
    target: Arc<GameTarget>,
    plan: Arc<deploy::DeployPlan>,
    compat: Option<CompatSummary>,
    temp_root: Result<Option<CompatLease>, InstallError>,
    progress: ProgressFn,
    overall: Arc<Mutex<OverallState>>,
    totals: deploy::StageTotals,
) -> Result<deploy::DeployReport, InstallError> {
    // Мир — первым: overrides слоя могут дописывать в `saves/<мир>/`
    // (datapacks), поэтому папка мира должна существовать до слоя.
    let world_dir_name = {
        let (game_root, plan, progress) = (game_root.clone(), plan.clone(), progress.clone());
        joined(
            tauri::async_runtime::spawn_blocking(move || {
                deploy::deploy_world(
                    &plan.zip_path,
                    &plan.world_prefix,
                    &plan.world_name,
                    &game_root.join("saves"),
                    &*progress,
                )
            })
            .await,
        )?
    };

    let mut report = deploy::DeployReport {
        world_dir_name: world_dir_name.clone(),
        ..Default::default()
    };
    // Вес контентной фазы (моды → ресурспаки → оверлей) известен заранее:
    // она идёт после слоя, но знаменатель стадии нужен до неё.
    let content_total = if plan.deploy_mods {
        totals.mods
    } else if !plan.resource_packs.is_empty() {
        totals.resourcepacks
    } else {
        totals.layers
    };
    let content_stage = if plan.deploy_mods { "mods" } else if !plan.resource_packs.is_empty() { "resourcepacks" } else { "layers" };
    enter_stage(&overall, content_stage, content_total, totals.world);

    // Слой — только в цель, созданную установщиком: фронт присылает план лишь
    // для свежей цели, а сервис слоя дополнительно проверяет, что `mods/`
    // целиком принадлежит плану (оборона в глубину).
    if let Some(plan_path) = plan.compat_plan_path.as_deref() {
        overall.lock().expect("overall progress state").begin_compat(&totals);
        if let Some(summary) = compat.as_ref() {
            // Контент карты главнее overrides сборки: запоминаем его пути в
            // плане, чтобы повтор слоя их не перетёр.
            let protected = temp_root.as_ref()
                .map_err(|e| InstallError::CompatApply(e.details()))
                .and_then(|root| root.as_ref().ok_or_else(|| InstallError::CompatApply("нет temp слоя".into())))
                .and_then(|root| deploy::content_paths(&plan, &world_dir_name)
            .and_then(|paths| {
                compat::protect_target_content_paths(&target, Path::new(plan_path), root, paths)
            }));
            let compat_report = match protected {
                Ok(()) => {
                    compat::apply_compat(&target, summary, Path::new(plan_path), temp_root.as_ref().expect("validated temp").as_ref().expect("compat temp"), progress.clone())
                        .await
                }
                Err(e) => CompatReport::failed(
                    Some(summary.kind),
                    summary.display_name.clone(),
                    summary.version.clone(),
                    compat::failure(&e),
                ),
            };
            report.compat = Some(compat_report);
        } else {
            report.compat = Some(CompatReport::failed(
                None,
                String::new(),
                String::new(),
                CompatFailure {
                    code: InstallError::CompatApply(String::new()).code().to_string(),
                    message: "сводка слоя не передана — слой не применён".to_string(),
                    details: String::new(),
                },
            ));
        }
        overall.lock().expect("overall progress state").finish_compat();
        enter_stage(&overall, content_stage, content_total, 0);
    }

    let content = {
        let compat_temp = temp_root.as_ref().ok().and_then(Option::as_ref).cloned();
        let (game_root, target, plan, progress, overall) = (
            game_root.clone(),
            target.clone(),
            plan.clone(),
            progress.clone(),
            overall.clone(),
        );
        move || -> Result<deploy::DeployReport, InstallError> {
            let mut report = report;
            if plan.deploy_mods {
                // Wipe — только для существующего чужого инстанса (wipe-экран
                // подтверждён) И только если в mods/ реально что-то есть.
                // Свежая цель всегда деплоится начисто без бэкапов.
                let wipe = plan.wipe_mods && !is_fresh_target(&target);
                let mods_report = deploy::deploy_mods(
                    &plan.zip_path,
                    &plan.content_prefix,
                    &game_root.join("mods"),
                    &deploy::backup_dir_for(&game_root),
                    wipe,
                    &*progress,
                )?;
                report.mods_written = mods_report.mods_written;
                report.backed_up.extend(mods_report.backed_up);
                report.skipped_identical = mods_report.skipped_identical;
                enter_stage(&overall, "resourcepacks", totals.resourcepacks, totals.mods);
            } else {
                enter_stage(&overall, "resourcepacks", totals.resourcepacks, 0);
            }
            // Loose-ресурспаки — в `resourcepacks/` (до оверлея: оверлей их
            // пропускает, чтобы не оставался дубль папки пака в корне игры).
            if !plan.resource_packs.is_empty() {
                let packs = deploy::deploy_resourcepacks(
                    &plan.zip_path,
                    &plan.resource_packs,
                    &game_root.join("resourcepacks"),
                    &*progress,
                    &mut |name| {
                        if let (Some(path), Some(temp)) = (plan.compat_plan_path.as_deref(), compat_temp.as_deref()) {
                            if let Err(error) = compat::protect_target_content_paths(
                                &target, Path::new(path), temp, vec![format!("resourcepacks/{name}")],
                            ) {
                                // Процессная защита уже сохранена; ошибка её дисковой копии
                                // не отменяет установку карты и не делает retry опасным.
                                tracing::warn!(code = error.code(), "не удалось сохранить защиту ресурспака в плане слоя");
                            }
                        }
                    },
                )?;
                report.resource_packs_written = packs;
                enter_stage(&overall, "layers", totals.layers, totals.resourcepacks);
            } else {
                enter_stage(&overall, "layers", totals.layers, 0);
            }
            // Оверлей кастомного контента — всегда: самописные скрипты/папки/
            // настройки автора обязаны встать, иначе карта сломана. Пустой
            // прогон дешёвый.
            let mut excluded_prefixes: Vec<String> = Vec::new();
            let mut excluded_files: Vec<String> = Vec::new();
            for p in &plan.resource_packs {
                match p.kind {
                    crate::domain::ResourcePackKind::Folder => {
                        excluded_prefixes.push(p.source.clone())
                    }
                    crate::domain::ResourcePackKind::Zip => excluded_files.push(p.source.clone()),
                }
            }
            let layers_report = deploy::deploy_layers(
                &plan.zip_path,
                &plan.world_prefix,
                &plan.content_prefix,
                plan.overrides_alias,
                &game_root,
                &report.world_dir_name,
                &excluded_prefixes,
                &excluded_files,
                &*progress,
            )?;
            report.backed_up.extend(layers_report.backed_up);
            Ok(report)
        }
    };
    let report = joined(tauri::async_runtime::spawn_blocking(content).await)?;
    progress(deploy::StageEvent::new("layers", totals.layers, totals.layers, String::new()));
    Ok(report)
}

/// Подготовка слоя совместимости (OptiFine). `E_COMPAT_UNAVAILABLE` — не
/// ошибка экрана: карта ставится без слоя, причина показывается в UI.
#[tauri::command]
pub async fn prepare_compat(
    app: tauri::AppHandle,
    mc_version: Option<String>,
    is_modded: bool,
) -> Cmd<CompatSummary> {
    let temp_root = compat_temp_root(&app).map_err(CmdError::from)?;
    compat::prepare_compat(mc_version.as_deref(), is_modded, &temp_root)
        .await
        .map_err(CmdError::from)
}

/// Повтор слоя в уже созданной свежей цели: мир не переустанавливается.
/// Гвард «только цель, созданная установщиком» — внутри сервиса: фронт мог
/// ошибиться, а слой в чужой инстанс ставить нельзя.
#[tauri::command]
pub async fn apply_compat(
    app: tauri::AppHandle,
    target: GameTarget,
    summary: CompatSummary,
    on_progress: tauri::ipc::Channel<deploy::StageEvent>,
) -> Cmd<CompatReport> {
    let temp_root = match compat_temp_root(&app) {
        Ok(root) => root,
        Err(error) => return Ok(CompatReport::failed(Some(summary.kind), summary.display_name,
            summary.version, compat::failure(&error))),
    };
    let progress = Arc::new(move |ev: deploy::StageEvent| {
        on_progress.send(ev).ok();
    });
    let report = compat::apply_compat(
        &target,
        &summary,
        Path::new(&summary.plan_path),
        &temp_root,
        progress,
    )
    .await;
    Ok(report)
}

/// Удаление плана слоя и его `.mrpack` после ухода с «Готово»/`reset()`.
#[tauri::command]
pub fn discard_compat(temp: tauri::State<'_, CompatTemp>, plan_path: String) -> usize {
    temp.current_root().map(|root| compat::discard_compat(Path::new(&plan_path), &root)).unwrap_or(0)
}

fn compat_temp_root(app: &tauri::AppHandle) -> Result<CompatLease, InstallError> {
    app.state::<CompatTemp>().root_in(&std::env::temp_dir())
        .map_err(|e| InstallError::CompatApply(format!("приватный temp: {e}")))
}

/// Свежая цель (только создана, `mods/` пуста) — wipe не нужен вообще.
/// Упрощение без костылей: wipe требуется, только если `mods/` непуста.
fn is_fresh_target(target: &GameTarget) -> bool {
    let game_root = normalize::game_root(target);
    let mods = game_root.join("mods");
    if !mods.is_dir() {
        return true;
    }
    std::fs::read_dir(&mods)
        .map(|mut r| r.next().is_none())
        .unwrap_or(true)
}

/// Подготовка версии TLauncher: поиск → провижининг официальным
/// установщиком → пометка [KK] → предвыбор в конфиге. Долгая операция,
/// прогресс — событиями `provision-progress` (тот же `StageEvent`).
#[tauri::command]
pub async fn ensure_tlauncher_version(
    app: tauri::AppHandle,
    data_dir: String,
    mc_version: String,
    loader: LoaderKind,
    loader_version: Option<String>,
) -> Cmd<vanilla::EnsureResult> {
    let data_dir = PathBuf::from(data_dir);
    // `%APPDATA%` (дом `.tlauncher` с конфигом и комплектным JRE).
    let roaming = dirs::config_dir();
    let progress = |ev: deploy::StageEvent| {
        app.emit("provision-progress", &ev).ok();
    };
    let result = tlauncher::ensure_version(
        &data_dir,
        roaming.as_deref(),
        &mc_version,
        loader,
        loader_version.as_deref(),
        &progress,
    )
    .await?;
    register_created(&result);
    Ok(result)
}

/// Подготовка версии Legacy Launcher. Отличие от TLauncher: `data_dir` — это
/// папка установки лаунчера (где `LL.exe` и `tl.properties`), а папка игры и
/// папка цели вычисляются из его конфига по выбранному режиму подпапок.
#[tauri::command]
pub async fn ensure_legacy_version(
    app: tauri::AppHandle,
    install_dir: String,
    mc_version: String,
    loader: LoaderKind,
    loader_version: Option<String>,
) -> Cmd<vanilla::EnsureResult> {
    let install_dir = PathBuf::from(install_dir);
    let progress = |ev: deploy::StageEvent| {
        app.emit("provision-progress", &ev).ok();
    };
    let config = legacy::LegacyConfig::load(&install_dir).ok_or(InstallError::NoDataDir {
        launcher: "Legacy Launcher (не найден tl.properties)".to_string(),
    })?;
    let result = legacy::ensure_version(
        &config,
        &mc_version,
        loader,
        loader_version.as_deref(),
        &progress,
    )
    .await?;
    register_created(&result);
    Ok(result)
}

/// Подготовка официального Minecraft Launcher: готовим версию в общем корне и
/// создаём СВОЮ установку с отдельной папкой игры под карту. Существующие
/// установки лаунчера не трогаем.
#[tauri::command]
pub async fn ensure_official_installation(
    app: tauri::AppHandle,
    data_dir: String,
    name: String,
    mc_version: String,
    loader: LoaderKind,
    loader_version: Option<String>,
    icon_path: Option<String>,
) -> Cmd<vanilla::EnsureResult> {
    let data_dir = PathBuf::from(data_dir);
    let icon = icon_path
        .as_deref()
        .and_then(|p| official::icon_data_uri(Some(Path::new(p))));
    let progress = |ev: deploy::StageEvent| {
        app.emit("provision-progress", &ev).ok();
    };
    let result = official::ensure_installation(
        &data_dir,
        &name,
        &mc_version,
        loader,
        loader_version.as_deref(),
        icon.as_deref(),
        &progress,
    )
    .await?;
    register_created(&result);
    Ok(result)
}

/// Пометить цель в реестре созданных установщиком: слой совместимости
/// допустим только в такую цель.
fn register_created(result: &vanilla::EnsureResult) {
    if result.created {
        provenance::register(&result.target);
    }
}

/// Прервать установку загрузчика (кнопка «Отмена» на экране прогресса).
///
/// Отдельный вызов, потому что `ensure_*` уже висит и вернуться может только
/// ошибкой: он заметит флаг и отдаст `E_CANCELLED`. Флаг сбрасывается на
/// старте каждого провижининга, поэтому «лишняя» отмена безвредна.
#[tauri::command]
pub fn cancel_provision() {
    crate::adapters::vanilla::cancel_provision();
}

/// Прописывает мир в панель Quick Play официального лаунчера — после успешного
/// деплоя, когда известно итоговое имя папки мира. Возвращает, включён ли
/// Quick Play в настройках лаунчера, чтобы фронт мог предупредить пользователя.
#[tauri::command]
pub async fn add_official_quick_play(
    data_dir: String,
    target: GameTarget,
    world_dir_name: String,
    world_name: String,
    icon_path: Option<String>,
) -> Cmd<bool> {
    let data_dir = PathBuf::from(data_dir);
    tauri::async_runtime::spawn_blocking(move || {
        // Ищем нашу установку по её папке игры: id нужен для `configId`.
        let inst = official::read_installations(&data_dir)
            .into_iter()
            .find(|i| i.dir_in(&data_dir) == target.path)
            .ok_or(InstallError::NoDataDir {
                launcher: "Minecraft Launcher (наша установка не найдена)".to_string(),
            })?;
        let icon = icon_path
            .as_deref()
            .and_then(|p| official::icon_data_uri(Some(Path::new(p))));
        official::add_quick_play(
            &data_dir,
            &inst.id,
            &world_dir_name,
            &world_name,
            icon.as_deref(),
        )
    })
    .await?
    .map_err(CmdError::from)
}

/// Удаление временных файлов завершённой установки (архив карты, иконки).
/// Принимает только строго наши имена внутри системного
/// temp — чужое и внешнее отклоняется (см. `temp_clean`). Best-effort:
/// возвращает число удалённых, установка от чистки не зависит.
#[tauri::command]
pub fn discard_temp_files(paths: Vec<String>) -> usize {
    crate::services::temp_clean::discard_install_files(paths)
}

/// Имя мира в `saves/` с учётом конфликта (для диалога «рядом/переименовать»).
/// Возвращает входное имя, если свободно, иначе `<Имя>_v2`... Замены нет.
#[tauri::command]
pub async fn suggest_world_name(target: GameTarget, world_name: String) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let game_root = match target.launcher {
            LauncherKind::Prism
            | LauncherKind::Pinecone
            | LauncherKind::Polymc
            | LauncherKind::Multimc => normalize::prism_game_root(&target.path),
            LauncherKind::Modrinth | LauncherKind::Tlauncher | LauncherKind::Legacy | LauncherKind::Official => {
                target.path.clone()
            }
        };
        let saves = game_root.join("saves");
        Ok(crate::domain::resolve_unique_name(
            &world_name,
            &|n| saves.join(n).exists(),
        ))
    })
    .await?
}

/// Кнопка «Играть»: Prism-семейство — прямой запуск в мир (у PolyMC/MultiMC
/// флаг `--world` опускается самим адаптером, см. `mmc_launch_args`),
/// Modrinth — открыть приложение на профиле, официальный — открыть
/// приложение (по AUMID для Store-сборки). Возвращает команду для «Подробнее».
#[tauri::command]
pub async fn launch_game(
    launcher: LauncherKind,
    exe: Option<String>,
    instance_id: String,
    world: Option<String>,
) -> Cmd<String> {
    tauri::async_runtime::spawn_blocking(move || match launcher {
        LauncherKind::Prism
        | LauncherKind::Pinecone
        | LauncherKind::Polymc
        | LauncherKind::Multimc => {
            let exe = exe
                .map(PathBuf::from)
                .ok_or(InstallError::NoLauncher)?;
            let id = instance_dir_name(&instance_id);
            prism::launch_mmc(&exe, launcher, &id, world.as_deref())
        }
        LauncherKind::Modrinth => {
            let exe = exe
                .map(PathBuf::from)
                .ok_or(InstallError::NoLauncher)?;
            modrinth::launch_modrinth(&exe)
        }
        LauncherKind::Tlauncher => {
            let exe = exe
                .map(PathBuf::from)
                .ok_or(InstallError::NoLauncher)?;
            tlauncher::launch_tlauncher(&exe)
        }
        LauncherKind::Legacy => {
            let exe = exe
                .map(PathBuf::from)
                .ok_or(InstallError::NoLauncher)?;
            // `load` сам разберётся с exe вместо папки установки.
            let config = legacy::LegacyConfig::load(&exe).ok_or_else(|| {
                InstallError::NoDataDir {
                    launcher: "Legacy Launcher (не найден tl.properties)".to_string(),
                }
            })?;
            // `instance_id` здесь — имя версии: Legacy выбирает её только
            // через `login.version`, своего CLI нет.
            legacy::launch_legacy(&config, &instance_id)
        }
        // У официального лаунчера нет CLI запуска: открываем приложение (по
        // AUMID, если Store-сборка) — профиль и так только что выбран нами.
        LauncherKind::Official => official::launch_official(exe.as_deref().map(Path::new)),
    })
    .await?
    .map_err(CmdError::from)
}

/// Имя папки инстанса из пути цели (для `--launch` нужен id/имя папки).
fn instance_dir_name(target_path: &str) -> String {
    PathBuf::from(target_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| target_path.to_string())
}

/// Иконка нового инстанса/профиля — только из архива (`pack.png`/`icon.png`).
/// Best-effort: неуспех — это `Ok(None)`, не ошибка.
#[tauri::command]
pub fn prepare_instance_icon(
    zip_path: String,
    content_prefix: String,
    world_prefix: String,
) -> Cmd<Option<String>> {
    Ok(download::prepare_instance_icon(zip_path, content_prefix, world_prefix))
}

/// Data URL иконки для превью в UI. Best-effort: неуспех — `Ok(None)`.
#[tauri::command]
pub async fn icon_data_url(path: String) -> Cmd<Option<String>> {
    Ok(crate::services::icon::data_url_for(std::path::Path::new(&path)))
}

/// Data URL иконки loose-ресурспака (`pack.png`) для превью на экране
/// проверки. Best-effort: неуспех — `Ok(None)`, установка идёт дальше.
#[tauri::command]
pub async fn resource_pack_icon(
    zip_path: String,
    source: String,
    kind: crate::domain::ResourcePackKind,
) -> Cmd<Option<String>> {
    Ok(crate::services::download::resource_pack_icon_data_url(
        std::path::Path::new(&zip_path),
        &source,
        kind,
    ))
}

/// Шаг входа: URL consent-страницы для системного браузера.
#[tauri::command]
pub async fn oauth_start() -> Cmd<serde_json::Value> {
    let url = tauri::async_runtime::spawn_blocking(auth::oauth_start_url)
        .await?
        .map_err(CmdError::from)?;
    Ok(serde_json::json!({ "url": url }))
}

/// Обмен `code` из `kraimaps://oauth/callback` на native-пару.
#[tauri::command]
pub async fn oauth_exchange(code: String, state: String) -> Cmd<()> {
    auth::oauth_exchange(&code, &state)
        .await
        .map_err(CmdError::from)
}

/// Профиль для шапки. Без сессии — `{user: null, is_authenticated: false}`.
#[tauri::command]
pub async fn auth_profile() -> Cmd<serde_json::Value> {
    download::auth_profile().await.map_err(CmdError::from)
}

/// Выход: revoke на сайте + чистка keyring.
#[tauri::command]
pub async fn auth_logout() -> Cmd<()> {
    auth::logout().await.map_err(CmdError::from)
}

/// Состояние хранилища сессии (I-4.2): переживёт ли сессия перезапуск.
/// `{persistent, detail}` — секретов нет. `persistent=false` (на Linux обычно
/// нет Secret Service) — фронт показывает предупреждение, а не тихий гость.
#[tauri::command]
pub fn auth_store_status() -> Cmd<serde_json::Value> {
    Ok(auth::store_status())
}

#[tauri::command]
pub async fn get_entitlement(map_id: i64) -> Cmd<download::EntitlementResponse> {
    download::get_entitlement(map_id)
        .await
        .map_err(CmdError::from)
}

#[tauri::command]
pub async fn get_manifest(map_id: i64) -> Cmd<download::InstallManifest> {
    download::get_manifest(map_id)
        .await
        .map_err(CmdError::from)
}

/// Скачивание файла карты во временную папку. Прогресс — событие
/// `site-download-progress`. Возвращает путь к проверенному файлу.
#[tauri::command]
pub async fn download_map_file(app: tauri::AppHandle, map_id: i64) -> Cmd<String> {
    download::download_map_file(&app, map_id)
        .await
        .map_err(CmdError::from)
}

#[tauri::command]
pub async fn install_start(req: download::InstallStartArgs) -> Cmd<download::InstallStartResponse> {
    download::install_start(req).await.map_err(CmdError::from)
}

#[tauri::command]
pub async fn install_complete(req: download::InstallCompleteArgs) -> Cmd<()> {
    download::install_complete(req)
        .await
        .map_err(CmdError::from)
}

/// Шаблон URL динамического сервера обновлений.
/// Переопределяется переменной окружения `KRAIMAPS_UPDATES_URL` (локальный
/// стенд): это полный URL; плейсхолдеры `{{current_version}}/{{target}}/{{arch}}`
/// подставляются автоматически, если в нём есть.
fn updates_template() -> String {
    if let Ok(custom) = std::env::var("KRAIMAPS_UPDATES_URL") {
        let custom = custom.trim().to_string();
        if !custom.is_empty() {
            return custom;
        }
    }
    format!(
        "{}/installer/updates?current={{{{current_version}}}}&target={{{{target}}}}&arch={{{{arch}}}}",
        site::api_base()
    )
}

/// Результат проверки обновлений для фронта.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    /// `current` — обновлений нет, молчим; `soft` — есть новее, но текущая
    /// поддерживается (оффер с отказом); `hard` — текущая ниже минимальной
    /// (блокировка работы, обхода нет).
    pub status: &'static str,
    pub latest: Option<String>,
    pub min_supported: Option<String>,
    pub notes: Option<String>,
}

/// Проверка обновлений: ручной GET динамического endpoint'а сайта + локальное
/// решение политики (`update_policy::decide`). Запрос публичный, без
/// авторизации: обновления доступны и гостям. Сеть недоступна →
/// `E_UPDATE_OFFLINE` (фронт решает: тихая автопроверка молчит, ручная
/// показывает тост). Блокировка — только по явному сигналу сервера.
#[tauri::command]
pub async fn updater_check() -> Cmd<UpdateCheckResult> {
    if !update_policy::platform_supported(std::env::consts::OS, std::env::consts::ARCH) {
        return Err(InstallError::UpdateCheckFailed(
            "автообновления пока доступны только для Windows x64".to_string(),
        )
        .into());
    }
    let url = update_policy::check_url(
        &updates_template(),
        site::INSTALLER_VERSION,
        update_policy::map_target(std::env::consts::OS),
        update_policy::map_arch(std::env::consts::ARCH),
    );
    let url: url::Url = url
        .parse()
        .map_err(|e| InstallError::UpdateCheckFailed(format!("updates endpoint: {e}")))?;
    if !update_policy::https_allowed(&url) {
        return Err(InstallError::UpdateCheckFailed(
            "URL обновлений должен быть HTTPS (HTTP — только для локального стенда)".to_string(),
        )
        .into());
    }
    let client = site::http_client().map_err(CmdError::from)?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::UpdateOffline(format!("updates check: {e}")))?;
    if resp.status() == reqwest::StatusCode::NO_CONTENT {
        // Сервер явно сказал «обновлений нет» — блокировать нечего.
        return Ok(UpdateCheckResult {
            status: "current",
            latest: None,
            min_supported: None,
            notes: None,
        });
    }
    if !resp.status().is_success() {
        return Err(InstallError::UpdateCheckFailed(format!(
            "updates check: http {}",
            resp.status()
        ))
        .into());
    }
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| InstallError::UpdateCheckFailed(format!("updates check: bad json: {e}")))?;
    // Понимаем и динамический ответ сайта, и запасной статический Tauri-JSON.
    let Some((version, server_min)) = update_policy::parse_server_info(&value) else {
        return Err(InstallError::UpdateCheckFailed(
            "updates check: bad json: unknown shape".to_string(),
        )
        .into());
    };
    let notes = value
        .get("notes")
        .or_else(|| value.get("changelog"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    match update_policy::decide(site::INSTALLER_VERSION, Some(&version), server_min.as_deref()) {
        update_policy::UpdateStatus::Current => Ok(UpdateCheckResult {
            status: "current",
            latest: None,
            min_supported: None,
            notes,
        }),
        update_policy::UpdateStatus::Soft { latest } => Ok(UpdateCheckResult {
            status: "soft",
            latest: Some(latest),
            min_supported: server_min,
            notes,
        }),
        update_policy::UpdateStatus::Hard { latest, min_supported } => Ok(UpdateCheckResult {
            status: "hard",
            latest,
            min_supported: Some(min_supported),
            notes,
        }),
    }
}

/// События прогресса установки обновления (канал `on_event`).
#[derive(Clone, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum UpdateDownloadEvent {
    #[serde(rename_all = "camelCase")]
    Started { content_length: Option<u64> },
    #[serde(rename_all = "camelCase")]
    Progress { chunk_length: usize },
    Finished,
}

/// Скачивание и установка обновления штатным плагином. Подпись сборки
/// проверяется плагином — установка без валидной подписи невозможна
/// (неуспех маппится в `E_UPDATE_SIGNATURE`). На Windows приложение
/// автоматически завершается при установке; рестарт делает фронт через
/// `plugin-process` после события `Finished`.
#[tauri::command]
pub async fn updater_install(
    app: tauri::AppHandle,
    on_event: tauri::ipc::Channel<UpdateDownloadEvent>,
) -> Cmd<()> {
    use tauri_plugin_updater::UpdaterExt;
    if !update_policy::platform_supported(std::env::consts::OS, std::env::consts::ARCH) {
        return Err(InstallError::UpdateCheckFailed(
            "автообновления пока доступны только для Windows x64".to_string(),
        )
        .into());
    }
    let endpoint: url::Url = updates_template().parse().map_err(|e| {
        InstallError::UpdateCheckFailed(format!("updates endpoint: invalid url: {e}"))
    })?;
    if !update_policy::https_allowed(&endpoint) {
        return Err(InstallError::UpdateCheckFailed(
            "URL обновлений должен быть HTTPS (HTTP — только для локального стенда)".to_string(),
        )
        .into());
    }
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| InstallError::UpdateCheckFailed(format!("updates endpoints: {e}")))?
        .build()
        .map_err(|e| InstallError::UpdateCheckFailed(format!("updates builder: {e}")))?;
    let update = match updater.check().await {
        Ok(update) => update,
        Err(e) => {
            // Транспорт лёг — отдельный код «нет соединения», остальное —
            // ошибка проверки. Типизированно: Reqwest-ошибка плагина.
            if let tauri_plugin_updater::Error::Reqwest(ref re) = e {
                if update_policy::is_offline_reqwest(re) {
                    return Err(InstallError::UpdateOffline(format!("updates check: {re}")).into());
                }
            }
            return Err(InstallError::UpdateCheckFailed(format!("updates check: {e}")).into());
        }
    };
    let Some(update) = update else {
        return Err(InstallError::UpdateCheckFailed(
            "обновление не найдено: уже установлена последняя версия".to_string(),
        )
        .into());
    };
    let mut started = false;
    update
        .download_and_install(
            |chunk_length, content_length| {
                if !started {
                    started = true;
                    let _ = on_event.send(UpdateDownloadEvent::Started { content_length });
                }
                let _ = on_event.send(UpdateDownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(UpdateDownloadEvent::Finished);
            },
        )
        .await
        .map_err(|e| {
            // Подпись не сошлась — типизированный матчинг по вариантам
            // ошибки плагина, не по подстрокам в тексте.
            if update_policy::is_signature_error(&e) {
                InstallError::UpdateSignature(format!("updates install: {e}"))
            } else {
                InstallError::UpdateInstallFailed(format!("updates install: {e}"))
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CompatFile, CompatKind, LauncherKind, ResourcePackKind, ResourcePackSelection};
    use crate::services::{http, work_budget::WorkBudget};
    use crate::test_utils::{compat_root, serve_http_once, serve_http_sequence, wait_for_entry_count, write_zip, TempDir};
    use sha2::Digest;

    fn map_plan(path: &Path) -> deploy::DeployPlan {
        deploy::DeployPlan {
            zip_path: path.to_path_buf(),
            world_prefix: "W/".into(),
            world_name: "W".into(),
            content_prefix: String::new(),
            overrides_alias: false,
            layers: Vec::new(),
            deploy_mods: false,
            wipe_mods: false,
            resource_packs: Vec::new(),
            compat_plan_path: None,
        }
    }

    fn target(path: &Path, loader: LoaderKind, loader_version: &str) -> GameTarget {
        GameTarget {
            launcher: LauncherKind::Official,
            name: "temporary test target".into(),
            path: path.to_path_buf(),
            minecraft_version: "1.20.1".into(),
            loader,
            loader_version: Some(loader_version.into()),
        }
    }

    fn compat_file(path: &str, url: String, body: &[u8]) -> CompatFile {
        CompatFile {
            path: path.into(), url, size: body.len() as u64,
            sha512: http::hex(sha2::Sha512::digest(body)), sha1: None,
        }
    }

    async fn prepare_fo_fixture(
        source: &Path,
        temp: &CompatLease,
        files: &[CompatFile],
        overrides: &[(&str, &[u8])],
    ) -> CompatSummary {
        let files: Vec<_> = files.iter().map(|file| serde_json::json!({
            "path": file.path, "fileSize": file.size,
            "hashes": { "sha512": file.sha512 }, "downloads": [file.url],
        })).collect();
        let index = serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1, "game": "minecraft",
            "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.14.23" },
            "files": files,
        })).expect("index");
        let pack_path = source.join("source.mrpack");
        let mut entries = vec![("modrinth.index.json", index.as_slice())];
        entries.extend_from_slice(overrides);
        write_zip(&pack_path, &entries);
        let bytes = std::fs::read(pack_path).expect("pack");
        let version_hash = http::hex(sha2::Sha512::digest(&bytes));
        let size = bytes.len() as u64;
        let (file_url, server) = serve_http_once(bytes, Duration::ZERO).await;
        let version = crate::adapters::modrinth_pack::FoVersion {
            version_number: "test-stable".into(), file_name: "FO.mrpack".into(),
            file_url, sha512: version_hash, size,
        };
        let summary = compat::prepare_fo_version("1.20.1", version, temp,
            &WorkBudget::within(Duration::from_secs(5))).await.expect("prepare FO");
        server.await.expect("pack server");
        summary
    }

    fn assert_finished_progress(events: &[deploy::StageEvent]) {
        for pair in events.windows(2) {
            let before = pair[0].overall_done.unwrap() as u128 * pair[1].overall_total.unwrap() as u128;
            let after = pair[1].overall_done.unwrap() as u128 * pair[0].overall_total.unwrap() as u128;
            assert!(after >= before, "progress went backwards: {:?} -> {:?}", pair[0], pair[1]);
        }
        let last = events.last().expect("completion event");
        assert_eq!(last.overall_done, last.overall_total, "a completed map must reach 100%");
    }

    async fn deploy_recorded(
        target: GameTarget,
        plan: deploy::DeployPlan,
        summary: CompatSummary,
        temp: CompatLease,
    ) -> (deploy::DeployReport, Vec<deploy::StageEvent>) {
        let totals = deploy::stage_totals(&plan, summary.total_bytes, summary.apply_bytes).expect("stage totals");
        let overall = Arc::new(Mutex::new(OverallState::new(&totals, &plan)));
        let events = Arc::new(Mutex::new(Vec::new()));
        let progress: ProgressFn = {
            let (overall, events) = (overall.clone(), events.clone());
            Arc::new(move |mut ev| {
                if !overall.lock().expect("overall").decorate(&mut ev) { return; }
                events.lock().expect("events").push(ev);
            })
        };
        let report = deploy_into(Arc::new(normalize::game_root(&target)), Arc::new(target),
            Arc::new(plan), Some(summary), Ok(Some(temp)), progress, overall, totals)
            .await.expect("map deployment");
        let events = events.lock().expect("events").clone();
        (report, events)
    }

    #[tokio::test]
    async fn fo_pipeline_progress_never_reuses_the_download_range_for_apply() {
        let source = TempDir::new("compat-progress-source");
        let game = TempDir::new("compat-progress-game");
        let temp = compat_root(&source);
        let body = b"layer jar".repeat(32);
        let (file_url, file_server) = serve_http_once(body.clone(), Duration::ZERO).await;
        let index = serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1, "game": "minecraft",
            "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.14.23" },
            "files": [{ "path": "mods/layer.jar", "fileSize": 288,
                "hashes": { "sha512": http::hex(sha2::Sha512::digest(&body)) },
                "downloads": [file_url] }],
        })).expect("index");
        let pack_path = source.join("source.mrpack");
        write_zip(&pack_path, &[("modrinth.index.json", &index),
            ("overrides/config/extra.cfg", &[b'x'; 120]),
            ("overrides/config/map.cfg", b"layer must not win")]);
        let pack_bytes = std::fs::read(&pack_path).expect("pack");
        let (pack_url, pack_server) = serve_http_once(pack_bytes.clone(), Duration::ZERO).await;
        let version = crate::adapters::modrinth_pack::FoVersion {
            version_number: "test-stable".into(), file_name: "FO.mrpack".into(),
            file_url: pack_url, sha512: http::hex(sha2::Sha512::digest(&pack_bytes)),
            size: pack_bytes.len() as u64,
        };
        let summary = compat::prepare_fo_version("1.20.1", version, &temp,
            &WorkBudget::within(Duration::from_secs(5))).await.expect("prepare FO");
        let map_zip = source.join("map.zip");
        write_zip(&map_zip, &[("W/level.dat", b"world"), ("config/map.cfg", b"map")]);
        let mut plan = map_plan(&map_zip);
        plan.compat_plan_path = Some(summary.plan_path.clone());
        let target = target(&game, LoaderKind::Fabric, "0.14.23");
        provenance::register(&target);

        let (report, events) = deploy_recorded(target, plan, summary, temp).await;
        pack_server.await.expect("pack server");
        file_server.await.expect("file server");

        assert!(report.compat.expect("compat report").error.is_none());
        assert_eq!(std::fs::read(game.join("config/map.cfg")).expect("map content"), b"map");
        assert_eq!(std::fs::read(game.join("config/extra.cfg")).expect("FO override").len(), 120);
        assert_finished_progress(&events);
        let last = events.last().expect("completion event");
        assert_eq!(last.overall_total, Some(704), "download, final apply bytes and map content are separate work");
    }

    #[tokio::test]
    async fn partial_download_failure_keeps_the_map_and_retry_preserves_conflict_names() {
        let source = TempDir::new("compat-download-retry-source");
        let game = TempDir::new("compat-download-retry-game");
        let temp = compat_root(&source);
        let body_a = b"first jar".to_vec();
        let body_b = b"second jar".to_vec();
        let (url_a, server_a) = serve_http_sequence(vec![body_a.clone(), body_a.clone()], Duration::ZERO).await;
        let (url_b, server_b) = serve_http_sequence(vec![vec![b'x'; body_b.len()], body_b.clone()], Duration::ZERO).await;
        let summary = prepare_fo_fixture(&source, &temp, &[
            compat_file("mods/a.jar", url_a, &body_a),
            compat_file("mods/b.jar", url_b, &body_b),
        ], &[
            ("overrides/config/extra.cfg", b"layer"),
            ("overrides/config/map.cfg", b"layer must not win"),
            ("overrides/saves/W_v2/level.dat", b"layer must not win"),
            ("client-overrides/resourcepacks/Story_v2/pack.mcmeta", b"layer must not win"),
        ]).await;
        let map_zip = source.join("map.zip");
        write_zip(&map_zip, &[
            ("W/level.dat", b"world"), ("config/map.cfg", b"map"),
            ("Story/pack.mcmeta", b"map pack"),
        ]);
        std::fs::create_dir_all(game.join("saves/W")).expect("existing world");
        std::fs::write(game.join("saves/W/level.dat"), b"user world").expect("existing world data");
        std::fs::create_dir_all(game.join("resourcepacks/Story")).expect("existing pack");
        std::fs::write(game.join("resourcepacks/Story/pack.mcmeta"), b"user pack").expect("existing pack data");
        let mut plan = map_plan(&map_zip);
        plan.compat_plan_path = Some(summary.plan_path.clone());
        plan.resource_packs = vec![ResourcePackSelection {
            source: "Story/".into(), name: "Story".into(), kind: ResourcePackKind::Folder,
        }];
        let target = target(&game, LoaderKind::Fabric, "0.14.23");
        provenance::register(&target);

        let (report, events) = tokio::time::timeout(Duration::from_secs(10),
            deploy_recorded(target.clone(), plan, summary.clone(), temp.clone()))
            .await.expect("failed layer must not hang map deployment");

        assert_eq!(report.compat.expect("non-fatal report").error.expect("hash failure").code, "E_COMPAT_INVALID");
        assert_eq!(report.world_dir_name, "W_v2");
        assert_eq!(report.resource_packs_written, vec!["Story_v2"]);
        assert_eq!(std::fs::read(game.join("saves/W/level.dat")).expect("old world"), b"user world");
        assert_eq!(std::fs::read(game.join("saves/W_v2/level.dat")).expect("new world"), b"world");
        assert!(!game.join("mods").exists(), "no layer file may be applied after a failed download");
        assert!(events.iter().any(|event| event.stage == "compat-download" && event.done_bytes > 0));
        assert!(!events.iter().any(|event| event.stage == "compat-apply"));
        assert_finished_progress(&events);
        wait_for_entry_count(&temp, 3).await;
        std::fs::write(game.join("saves/W_v2/level.dat"), b"player progress").expect("played world");
        std::fs::remove_file(map_zip).expect("map archive no longer needed for retry");

        let retry = tokio::time::timeout(Duration::from_secs(10), compat::apply_compat(
            &target, &summary, Path::new(&summary.plan_path), &temp, Arc::new(|_| {})))
            .await.expect("retry must finish");
        server_a.await.expect("first jar server");
        server_b.await.expect("second jar server");

        assert!(retry.error.is_none(), "retry: {:?}", retry.error);
        assert_eq!(std::fs::read(game.join("mods/a.jar")).expect("first jar"), body_a);
        assert_eq!(std::fs::read(game.join("mods/b.jar")).expect("second jar"), body_b);
        assert_eq!(std::fs::read(game.join("config/extra.cfg")).expect("layer content"), b"layer");
        assert_eq!(std::fs::read(game.join("config/map.cfg")).expect("map content"), b"map");
        assert_eq!(std::fs::read(game.join("saves/W_v2/level.dat")).expect("played world"), b"player progress");
        assert_eq!(std::fs::read(game.join("resourcepacks/Story/pack.mcmeta")).expect("user pack"), b"user pack");
        assert_eq!(std::fs::read(game.join("resourcepacks/Story_v2/pack.mcmeta")).expect("map pack"), b"map pack");
        wait_for_entry_count(&temp, 3).await;
    }

    #[tokio::test]
    async fn partial_apply_failure_keeps_committed_files_and_retry_does_not_reinstall_the_world() {
        let source = TempDir::new("compat-apply-retry-source");
        let game = TempDir::new("compat-apply-retry-game");
        let temp = compat_root(&source);
        let body = b"layer jar".to_vec();
        let (url, server) = serve_http_sequence(vec![body.clone(), body.clone()], Duration::ZERO).await;
        let summary = prepare_fo_fixture(&source, &temp,
            &[compat_file("mods/layer.jar", url, &body)], &[
                ("overrides/config/extra.cfg", b"layer"),
                ("overrides/config/map.cfg", b"layer must not win"),
                ("overrides/z-blocked.txt", b"last layer file"),
            ]).await;
        let map_zip = source.join("map.zip");
        write_zip(&map_zip, &[("W/level.dat", b"world"), ("config/map.cfg", b"map"),
            (" readme.txt", b"map instructions")]);
        std::fs::create_dir_all(game.join("config")).expect("config");
        std::fs::write(game.join("config/extra.cfg"), b"original").expect("original config");
        std::fs::create_dir(game.join("z-blocked.txt")).expect("write obstruction");
        let mut plan = map_plan(&map_zip);
        plan.compat_plan_path = Some(summary.plan_path.clone());
        let target = target(&game, LoaderKind::Fabric, "0.14.23");
        provenance::register(&target);

        let (report, events) = tokio::time::timeout(Duration::from_secs(10),
            deploy_recorded(target.clone(), plan, summary.clone(), temp.clone()))
            .await.expect("write failure must not hang map deployment");

        assert_eq!(report.compat.expect("non-fatal report").error.expect("write failure").code, "E_COMPAT_APPLY");
        assert_eq!(std::fs::read(game.join("saves/W/level.dat")).expect("world"), b"world");
        assert_eq!(std::fs::read(game.join("config/map.cfg")).expect("map content"), b"map");
        assert_eq!(std::fs::read(game.join("config/extra.cfg")).expect("committed config"), b"layer");
        assert_eq!(std::fs::read(game.join("mods/layer.jar")).expect("committed mod"), body);
        assert!(events.iter().any(|event| event.stage == "compat-apply" && event.done_bytes > 0));
        assert_finished_progress(&events);
        wait_for_entry_count(&temp, 3).await;
        std::fs::remove_dir(game.join("z-blocked.txt")).expect("remove fixture obstruction");
        std::fs::write(game.join("saves/W/level.dat"), b"player progress").expect("played world");
        std::fs::remove_file(map_zip).expect("map archive no longer needed for retry");

        let retry = tokio::time::timeout(Duration::from_secs(10), compat::apply_compat(
            &target, &summary, Path::new(&summary.plan_path), &temp, Arc::new(|_| {})))
            .await.expect("retry must finish");
        server.await.expect("jar server");

        assert!(retry.error.is_none(), "retry: {:?}", retry.error);
        assert_eq!(std::fs::read(game.join("z-blocked.txt")).expect("retried file"), b"last layer file");
        assert_eq!(std::fs::read(game.join("saves/W/level.dat")).expect("played world"), b"player progress");
        assert_eq!(std::fs::read(game.join("config/map.cfg")).expect("map content"), b"map");
        assert_eq!(std::fs::read(game.join(" readme.txt")).expect("map instructions"), b"map instructions");
        let backups = crate::services::safe_fs::SafeDir::open(&game).expect("game directory")
            .files().expect("game files");
        assert!(backups.iter().any(|entry| entry.path.starts_with(".kraimaps-backup")
            && entry.path.ends_with("config/extra.cfg")
            && std::fs::read(game.join(&entry.path)).expect("backup") == b"original"));
        wait_for_entry_count(&temp, 3).await;
    }

    #[tokio::test]
    async fn failed_compat_does_not_leave_unperformed_bytes_in_the_final_denominator() {
        let source = TempDir::new("compat-failed-progress-source");
        let game = TempDir::new("compat-failed-progress-game");
        let temp = compat_root(&source);
        let map_zip = source.join("map.zip");
        write_zip(&map_zip, &[("W/level.dat", b"world")]);
        let summary = CompatSummary {
            kind: CompatKind::FabulouslyOptimized, display_name: "FO".into(), version: "test".into(),
            loader: LoaderKind::Fabric, loader_version: "0.14.23".into(), mc_version: "1.20.1".into(),
            total_bytes: Some(100), apply_bytes: Some(100), files_count: 1, note: None,
            plan_path: temp.join("compat-plan-missing.json").to_string_lossy().into_owned(),
        };
        let mut plan = map_plan(&map_zip);
        plan.compat_plan_path = Some(summary.plan_path.clone());
        let target = target(&game, LoaderKind::Fabric, "0.14.23");
        provenance::register(&target);

        let (report, events) = deploy_recorded(target, plan, summary, temp).await;

        assert!(report.compat.expect("non-fatal report").error.is_some());
        assert_eq!(std::fs::read(game.join("saves/W/level.dat")).expect("installed world"), b"world");
        let last = events.last().expect("completion event");
        assert_eq!(last.overall_done, last.overall_total, "non-fatal failure must still finish map progress");
        assert_eq!(last.overall_total, Some(5), "failed work must not count as performed");
    }

    #[test]
    fn unknown_optifine_bytes_do_not_change_the_known_overall_range() {
        let totals = deploy::StageTotals { world: 5, layers: 3, ..Default::default() };
        let mut state = OverallState::new(&totals, &map_plan(Path::new("unused.zip")));
        let overall = Mutex::new(state);
        enter_stage(&overall, "compat-download", 0, 5);
        state = overall.into_inner().expect("overall");
        state.begin_compat(&totals);
        for stage in ["compat-download", "compat-apply"] {
            let mut event = deploy::StageEvent::new(stage, 1024, 4096, "OptiFine.jar".into());
            assert!(state.decorate(&mut event));
            assert_eq!((event.overall_done, event.overall_total), (Some(5), Some(8)));
        }
    }

    #[test]
    fn callbacks_from_a_finished_compat_attempt_cannot_change_progress() {
        let totals = deploy::StageTotals {
            world: 5, layers: 3, compat: Some(100), compat_apply: Some(60), ..Default::default()
        };
        for (stage, done, stage_total, final_total) in [
            ("compat-download", 40, 100, 48),
            ("compat-apply", 20, 60, 128),
        ] {
            let state = OverallState::new(&totals, &map_plan(Path::new("unused.zip")));
            let overall = Mutex::new(state);
            enter_stage(&overall, "compat-download", 100, 5);
            let mut state = overall.into_inner().expect("overall");
            state.begin_compat(&totals);
            if stage == "compat-apply" {
                assert!(state.decorate(&mut deploy::StageEvent::new("compat-download", 100, 100, String::new())));
            }
            assert!(state.decorate(&mut deploy::StageEvent::new(stage, done, stage_total, String::new())));
            state.finish_compat();

            let mut late = deploy::StageEvent::new(stage, stage_total, stage_total, "late worker".into());
            assert!(!state.decorate(&mut late), "finished attempt accepted {stage} callback");

            let overall = Mutex::new(state);
            enter_stage(&overall, "layers", 3, 0);
            let mut last = deploy::StageEvent::new("layers", 3, 3, String::new());
            assert!(overall.lock().expect("overall").decorate(&mut last));
            assert_eq!((last.overall_done, last.overall_total), (Some(final_total), Some(final_total)));
        }
    }
}
