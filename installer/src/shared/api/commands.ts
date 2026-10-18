/** Типизированные обёртки над Tauri-командами (src-tauri/src/commands.rs). */
import { Channel, invoke } from '@tauri-apps/api/core';
import type {
  Detection,
  DeployPlan,
  DeployReport,
  EnsureResult,
  GameTarget,
  LauncherKind,
  LoaderKind,
  PackAnalysis,
  ResourcePackKind,
} from '../types/installer';
import type {
  EntitlementResponse,
  InstallCompleteRequest,
  InstallManifest,
  InstallStartRequest,
  InstallStartResponse,
  UserProfileResponse,
} from '../types/site';
import type { UpdateCheckResult, UpdateDownloadEvent } from '../types/updater';

export const api = {
  ping(): Promise<string> {
    return invoke('ping');
  },

  /**
   * Единая версия установщика из Rust (`site::INSTALLER_VERSION`).
   * Она же уходит в User-Agent и в `install-start` как `installer_version`.
   */
  getInstallerVersion(): Promise<string> {
    return invoke('get_installer_version');
  },

  analyzePack(path: string): Promise<PackAnalysis> {
    return invoke('analyze_pack', { path });
  },

  detectLaunchers(manualRoots: string[], knownRoots: string[]): Promise<Detection> {
    return invoke('detect_launchers', { manualRoots, knownRoots });
  },

  resolveLoaderVersion(
    loader: LoaderKind,
    mcVersion: string,
    hint: string | null,
  ): Promise<string | null> {
    return invoke('resolve_loader_version', { loader, mcVersion, hint });
  },

  /** Прервать установку загрузчика. Уже висящий вызов `ensure_*` отдаст
   * `E_CANCELLED`; сам вызов возвращается сразу. */
  cancelProvision(): Promise<void> {
    return invoke('cancel_provision');
  },

  createInstance(args: {
    launcher: LauncherKind;
    dataDir: string;
    name: string;
    mcVersion: string;
    loader: LoaderKind;
    loaderVersion: string | null;
    iconPath: string | null;
  }): Promise<GameTarget> {
    return invoke('create_instance', args);
  },

  /**
   * Подготовка версии TLauncher (поиск → провижининг официальным
   * установщиком → пометка [KK] → предвыбор). Прогресс — событие
   * `provision-progress` (тот же StageEvent).
   */
  ensureTlauncherVersion(args: {
    dataDir: string;
    mcVersion: string;
    loader: LoaderKind;
    loaderVersion: string | null;
  }): Promise<EnsureResult> {
    return invoke('ensure_tlauncher_version', args);
  },

  /**
   * Подготовка версии Legacy Launcher. `installDir` — папка установки
   * лаунчера (где `LL.exe` и `tl.properties`): он сам скажет, где папка
   * игры и какой режим подпапок выбран.
   */
  ensureLegacyVersion(args: {
    installDir: string;
    mcVersion: string;
    loader: LoaderKind;
    loaderVersion: string | null;
  }): Promise<EnsureResult> {
    return invoke('ensure_legacy_version', args);
  },

  /**
   * Официальный Minecraft Launcher: готовим версию в общем корне и создаём
   * СВОЮ установку с отдельной папкой игры под карту. Установки пользователя
   * не трогаем.
   */
  ensureOfficialInstallation(args: {
    dataDir: string;
    name: string;
    mcVersion: string;
    loader: LoaderKind;
    loaderVersion: string | null;
    iconPath: string | null;
  }): Promise<EnsureResult> {
    return invoke('ensure_official_installation', args);
  },

  /**
   * Прописывает мир в панель Quick Play (после успешного деплоя).
   * Возвращает, включён ли Quick Play в настройках лаунчера.
   */
  addOfficialQuickPlay(args: {
    dataDir: string;
    target: GameTarget;
    worldDirName: string;
    worldName: string;
    iconPath: string | null;
  }): Promise<boolean> {
    return invoke('add_official_quick_play', args);
  },

  /**
   * Иконка нового инстанса из архива (`pack.png`/`icon.png`).
   * Best-effort: неуспех — это `null`, установка идёт дальше.
   */
  prepareInstanceIcon(
    zipPath: string,
    contentPrefix: string,
    worldPrefix: string,
  ): Promise<string | null> {
    return invoke('prepare_instance_icon', { zipPath, contentPrefix, worldPrefix });
  },

  /** Data URL файла иконки для превью (`<img>` без file-протокола). */
  iconDataUrl(path: string): Promise<string | null> {
    return invoke('icon_data_url', { path });
  },

  /** Data URL иконки ресурспака (pack.png) для экрана проверки. Best-effort. */
  resourcePackIcon(
    zipPath: string,
    source: string,
    kind: ResourcePackKind,
  ): Promise<string | null> {
    return invoke('resource_pack_icon', { zipPath, source, kind });
  },

  deployPack(target: GameTarget, plan: DeployPlan): Promise<DeployReport> {
    return invoke('deploy_pack', { target, plan });
  },

  /**
   * Закрывает запущенный лаунчер на время установки. Возвращает, был ли он
   * запущен (фронт по этому флагу решает, открывать ли его обратно).
   * Никогда не реджектится в ошибку установки.
   */
  closeLauncher(launcher: LauncherKind): Promise<boolean> {
    return invoke('close_launcher', { launcher });
  },

  /**
   * Открывает лаунчер после установки (только если был запущен на старте).
   * `launcher` обязателен: у Store-сборки официального лаунчера exe нет вовсе,
   * и запускать его надо по AUMID — по одному `exe` его не открыть.
   * Best-effort: неуспех глотает вызыватель — установка уже успешна.
   */
  reopenLauncher(launcher: LauncherKind, exe: string | null): Promise<string> {
    return invoke('reopen_launcher', { launcher, exe });
  },

  suggestWorldName(target: GameTarget, worldName: string): Promise<string> {
    return invoke('suggest_world_name', { target, worldName });
  },

  launchGame(args: {
    launcher: LauncherKind;
    exe: string | null;
    instanceId: string;
    world: string | null;
    folder: string | null;
  }): Promise<string> {
    return invoke('launch_game', args);
  },

  oauthStart(): Promise<{ url: string }> {
    return invoke('oauth_start');
  },

  oauthExchange(code: string, state: string): Promise<void> {
    return invoke('oauth_exchange', { code, state });
  },

  authProfile(): Promise<UserProfileResponse> {
    return invoke('auth_profile');
  },

  authLogout(): Promise<void> {
    return invoke('auth_logout');
  },

  /**
   * Состояние хранилища сессии (I-4.2): переживёт ли сессия перезапуск.
   * `persistent=false` (на Linux обычно нет Secret Service) — фронт показывает
   * предупреждение, а не тихий гость. Секретов в ответе нет.
   */
  authStoreStatus(): Promise<{ persistent: boolean; detail: string }> {
    return invoke('auth_store_status');
  },

  getEntitlement(mapId: number): Promise<EntitlementResponse> {
    return invoke('get_entitlement', { mapId });
  },

  getManifest(mapId: number): Promise<InstallManifest> {
    return invoke('get_manifest', { mapId });
  },

  downloadMapFile(mapId: number): Promise<string> {
    return invoke('download_map_file', { mapId });
  },

  installStart(req: InstallStartRequest): Promise<InstallStartResponse> {
    return invoke('install_start', { req });
  },

  installComplete(req: InstallCompleteRequest): Promise<void> {
    return invoke('install_complete', { req });
  },

  /**
   * Удаление временных файлов завершённой установки (архив карты, иконки).
   * Best-effort: возвращает число удалённых. Бэкенд принимает только строго
   * наши имена внутри системного temp — чужое удалить этой командой нельзя.
   */
  discardTempFiles(paths: string[]): Promise<number> {
    return invoke('discard_temp_files', { paths });
  },

  /**
   * Проверка обновлений у динамического сервера сайта.
   * Публичный запрос без авторизации. Офлайн → реджект `E_UPDATE_OFFLINE`.
   */
  updaterCheck(): Promise<UpdateCheckResult> {
    return invoke('updater_check');
  },

  /**
   * Скачивание и установка обновления штатным плагином.
   * Прогресс — через колбэк (канал `on_event`). На Windows приложение
   * завершится само при установке; рестарт — отдельно через plugin-process.
   */
  updaterInstall(onEvent: (event: UpdateDownloadEvent) => void): Promise<void> {
    const channel = new Channel<UpdateDownloadEvent>();
    channel.onmessage = onEvent;
    return invoke('updater_install', { onEvent: channel });
  },
};
