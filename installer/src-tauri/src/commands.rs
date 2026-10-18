//! Tauri-команды: тонкий мост между движком (`services`/`adapters`)
//! и фронтендом. Никакой логики здесь нет — только сериализация,
//! `spawn_blocking` для тяжёлых файловых операций и проброс прогресса
//! в событие `deploy-progress`. Ошибки — объектом
//! `{code, message, details}` (код `E_*` для «Подробнее»).

use crate::adapters::{legacy, manual, modrinth, official, prism, tlauncher, vanilla};
use crate::domain::{
    GameTarget, InstallError, LauncherKind, LoaderKind, PackAnalysis,
};
use crate::services::{auth, deploy, detect, download, normalize, site, update_policy, versions};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::Emitter;

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

/// Детект лаунчеров. `manual_roots` — ручные пути из store;
/// `known_roots` — ранее найденные exe/корни (запоминает фронтенд),
/// чтобы портативка находилась и без запуска.
#[tauri::command]
pub async fn detect_launchers(
    manual_roots: Vec<String>,
    known_roots: Vec<String>,
) -> Cmd<detect::Detection> {
    let manuals: Vec<PathBuf> = manual_roots.into_iter().map(PathBuf::from).collect();
    let known: Vec<PathBuf> = known_roots.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || detect::detect_launchers(&manuals, &known))
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
    tauri::async_runtime::spawn_blocking(move || match launcher {
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
        LauncherKind::Manual => Err(InstallError::NoLauncher),
        // У TLauncher и Legacy Launcher нет инстансов: версии готовятся
        // командами `ensure_tlauncher_version` / `ensure_legacy_version`.
        LauncherKind::Tlauncher | LauncherKind::Legacy => Err(InstallError::NoLauncher),
        // У официального лаунчера своя модель (Установки) — отдельная команда.
        LauncherKind::Official => Err(InstallError::NoLauncher),
    })
    .await?
    .map_err(CmdError::from)
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
        LauncherKind::Manual => Err(InstallError::NoLauncher),
        _ => {
            let exe = exe.ok_or(InstallError::NoLauncher)?;
            detect::reopen_launcher(&exe)
        }
    })
    .await?
    .map_err(CmdError::from)
}

/// Прямой деплой модели в цель. Прогресс — событиями `deploy-progress`.
#[tauri::command]
pub async fn deploy_pack(
    app: tauri::AppHandle,
    target: GameTarget,
    plan: deploy::DeployPlan,
) -> Cmd<deploy::DeployReport> {
    // Корень игры зависит от лаунчера; Modrinth перепроверяем на запись.
    let game_root = match target.launcher {
        LauncherKind::Prism
        | LauncherKind::Pinecone
        | LauncherKind::Polymc
        | LauncherKind::Multimc => normalize::prism_game_root(&target.path),
        // Modrinth: профиль; TLauncher и ручная папка: сам game dir.
        // Деплой пишет только файлы профиля (saves/mods/config), базу не
        // трогает — запущенное приложение не мешает, запрета нет.
        LauncherKind::Modrinth | LauncherKind::Manual | LauncherKind::Tlauncher | LauncherKind::Legacy | LauncherKind::Official => {
            target.path.clone()
        }
    };
    tauri::async_runtime::spawn_blocking(move || {
        // Сквозной прогресс всего деплоя: знаменатель считаем один раз
        // до старта (байты реально выполняемых стадий), числитель —
        // смещение завершённых стадий + байты текущей. Полоса идёт только
        // вперёд и честно отражает объём работы.
        // События троттлим до ~20/с: сотни мелких файлов иначе спамят
        // в IPC, а CSS-переход ширины вечно догоняет — полоса визуально
        // стоит на месте, хотя проценты растут.
        let totals = deploy::stage_totals(&plan)?;
        let overall_total = totals.overall(plan.deploy_mods, !plan.resource_packs.is_empty());
        let overall = std::sync::Mutex::new(OverallState {
            base: 0,
            stage_total: totals.world,
            total: overall_total,
            last_emit: std::time::Instant::now() - std::time::Duration::from_secs(1),
        });
        let progress = |mut ev: deploy::StageEvent| {
            let mut st = overall.lock().expect("overall progress state");
            let capped = ev.done_bytes.min(st.stage_total);
            if st.total > 0 {
                ev.overall_done = Some(st.base + capped);
                ev.overall_total = Some(st.total);
            }
            // Финал стадии доставляем всегда (иначе 100% стадии и всего
            // деплоя могут потеряться в троттлинге).
            let finished = ev.done_bytes >= ev.total_bytes;
            let now = std::time::Instant::now();
            if finished || now.duration_since(st.last_emit) >= std::time::Duration::from_millis(50)
            {
                st.last_emit = now;
                app.emit("deploy-progress", &ev).ok();
            }
        };
        deploy_into(&game_root, &target, &plan, &progress, &overall, &totals)
    })
    .await?
    .map_err(CmdError::from)
}

/// Смещение сквозного прогресса: сколько байт завершённых стадий уже
/// позади (`base`), вес текущей стадии и общий знаменатель.
struct OverallState {
    base: u64,
    stage_total: u64,
    total: u64,
    last_emit: std::time::Instant,
}

/// Переключение обёртки прогресса на следующую стадию.
fn enter_stage(overall: &std::sync::Mutex<OverallState>, stage_total: u64, finished: u64) {
    let mut st = overall.lock().expect("overall progress state");
    st.base += finished;
    st.stage_total = stage_total;
}

fn deploy_into(
    game_root: &PathBuf,
    target: &GameTarget,
    plan: &deploy::DeployPlan,
    progress: deploy::Progress,
    overall: &std::sync::Mutex<OverallState>,
    totals: &deploy::StageTotals,
) -> Result<deploy::DeployReport, InstallError> {
    let saves_dir = game_root.join("saves");
    let world_dir_name = deploy::deploy_world(
        &plan.zip_path,
        &plan.world_prefix,
        &plan.world_name,
        &saves_dir,
        progress,
    )?;
    enter_stage(overall, totals.mods, totals.world);

    let mut report = deploy::DeployReport {
        world_dir_name: world_dir_name.clone(),
        ..Default::default()
    };
    if plan.deploy_mods {
        // Wipe — только для существующего чужого инстанса (wipe-экран
        // подтверждён) И только если в mods/ реально что-то есть.
        // Свежая цель всегда деплоится начисто без бэкапов.
        let wipe = plan.wipe_mods && !is_fresh_target(target);
        let mods_report = deploy::deploy_mods(
            &plan.zip_path,
            &plan.content_prefix,
            &game_root.join("mods"),
            &deploy::backup_dir_for(game_root),
            wipe,
            progress,
        )?;
        report.mods_written = mods_report.mods_written;
        report.backed_up.extend(mods_report.backed_up);
        report.skipped_identical = mods_report.skipped_identical;
        enter_stage(overall, totals.resourcepacks, totals.mods);
    } else {
        enter_stage(overall, totals.resourcepacks, 0);
    }
    // Loose-ресурспаки — в `resourcepacks/` (до оверлея: оверлей их пропускает,
    // чтобы не оставался дубль папки пака в корне игры).
    if !plan.resource_packs.is_empty() {
        let packs = deploy::deploy_resourcepacks(
            &plan.zip_path,
            &plan.resource_packs,
            &game_root.join("resourcepacks"),
            progress,
        )?;
        report.resource_packs_written = packs;
        enter_stage(overall, totals.layers, totals.resourcepacks);
    } else {
        enter_stage(overall, totals.layers, 0);
    }
    // Оверлей кастомного контента — всегда: самописные скрипты/папки/настройки
    // автора обязаны встать, иначе карта сломана. Пустой прогон дешёвый.
    let mut excluded_prefixes: Vec<String> = Vec::new();
    let mut excluded_files: Vec<String> = Vec::new();
    for p in &plan.resource_packs {
        match p.kind {
            crate::domain::ResourcePackKind::Folder => excluded_prefixes.push(p.source.clone()),
            crate::domain::ResourcePackKind::Zip => excluded_files.push(p.source.clone()),
        }
    }
    let layers_report = deploy::deploy_layers(
        &plan.zip_path,
        &plan.world_prefix,
        &plan.content_prefix,
        plan.overrides_alias,
        game_root,
        &world_dir_name,
        &excluded_prefixes,
        &excluded_files,
        progress,
    )?;
    report.backed_up.extend(layers_report.backed_up);
    Ok(report)
}

/// Свежая цель (только создана, `mods/` пуста) — wipe не нужен вообще.
/// Упрощение без костылей: wipe требуется, только если `mods/` непуста.
fn is_fresh_target(target: &GameTarget) -> bool {
    let game_root = match target.launcher {
        LauncherKind::Prism
        | LauncherKind::Pinecone
        | LauncherKind::Polymc
        | LauncherKind::Multimc => normalize::prism_game_root(&target.path),
        _ => target.path.clone(),
    };
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
    tlauncher::ensure_version(
        &data_dir,
        roaming.as_deref(),
        &mc_version,
        loader,
        loader_version.as_deref(),
        &progress,
    )
    .await
    .map_err(CmdError::from)
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
    legacy::ensure_version(
        &config,
        &mc_version,
        loader,
        loader_version.as_deref(),
        &progress,
    )
    .await
    .map_err(CmdError::from)
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
    official::ensure_installation(
        &data_dir,
        &name,
        &mc_version,
        loader,
        loader_version.as_deref(),
        icon.as_deref(),
        &progress,
    )
    .await
    .map_err(CmdError::from)
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
            LauncherKind::Modrinth | LauncherKind::Manual | LauncherKind::Tlauncher | LauncherKind::Legacy | LauncherKind::Official => {
                target.path.clone()
            }
        };
        let saves = game_root.join("saves");
        Ok(crate::domain::resolve_world_name(
            &world_name,
            &|n| saves.join(n).exists(),
        ))
    })
    .await?
}

/// Кнопка «Играть»: Prism-семейство — прямой запуск в мир (у PolyMC/MultiMC
/// флаг `--world` опускается самим адаптером, см. `mmc_launch_args`),
/// Modrinth — открыть приложение на профиле, ручная папка — открыть папку
/// в проводнике. Возвращает команду для «Подробнее».
#[tauri::command]
pub async fn launch_game(
    launcher: LauncherKind,
    exe: Option<String>,
    instance_id: String,
    world: Option<String>,
    folder: Option<String>,
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
        LauncherKind::Manual => {
            let folder = folder
                .map(PathBuf::from)
                .ok_or(InstallError::NoLauncher)?;
            manual::open_folder(&folder)
        }
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
