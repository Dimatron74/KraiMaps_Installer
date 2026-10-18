/** TS-зеркала доменных типов Rust (src-tauri/src/domain). */

export type LauncherKind =
  | 'prism'
  | 'pinecone'
  | 'polymc'
  | 'multimc'
  | 'modrinth'
  | 'manual'
  | 'tlauncher'
  | 'legacy'
  | 'official';
export type LoaderKind = 'vanilla' | 'fabric' | 'forge' | 'quilt' | 'neoforge';

export interface ModInfo {
  file_name: string;
  mod_id: string;
  version: string | null;
  loader: LoaderKind;
  size_bytes: number;
}

export interface PackAnalysis {
  file_name: string;
  file_size_bytes: number;
  sha256: string;
  world_name: string;
  world_version: string | null;
  is_modded: boolean;
  loader: LoaderKind;
  loader_version_hint: string | null;
  mods: ModInfo[];
  layers: string[];
  total_uncompressed_bytes: number;
  entry_count: number;
  world_prefix: string;
  /** Корень контента (обёртка) или "" для плоских архивов. */
  content_prefix: string;
  /** Alias overrides/ (упакованный модпак). */
  overrides_alias: boolean;
  /** Loose-ресурспаки рядом с миром (вне мира и вне resourcepacks/). */
  resource_packs: ResourcePackInfo[];
}

/** Вид ресурспака в архиве: распакованная папка или zip-файл. */
export type ResourcePackKind = 'folder' | 'zip';

/** Ресурспак, найденный рядом с миром. Маркер — pack.mcmeta (+assets/). */
export interface ResourcePackInfo {
  /** Имя для показа: папка — basename, zip — имя файла с .zip. */
  name: string;
  /** Источник в архиве: папка — префикс с / на конце, zip — путь записи. */
  source: string;
  kind: ResourcePackKind;
  /** pack.description из pack.mcmeta (может отсутствовать). */
  description: string | null;
}

/** Включённый пользователем пак для плана деплоя. */
export interface ResourcePackSelection {
  source: string;
  name: string;
  kind: ResourcePackKind;
}

export interface GameTarget {
  launcher: LauncherKind;
  name: string;
  /** Корень цели: инстанс Prism / профиль Modrinth / папка вручную. */
  path: string;
  minecraft_version: string;
  loader: LoaderKind;
  loader_version: string | null;
}

export interface LauncherInfo {
  kind: LauncherKind;
  display_name: string;
  exe_path: string | null;
  data_dir: string;
  /** Данные рядом с exe, а не в стандартном месте. */
  portable: boolean;
  targets: GameTarget[];
  /**
   * Приложение найдено (exe на месте или есть запись в списке установленных
   * программ). `false` — осталась только папка с данными: карту поставить
   * можно, а запустить лаунчер нечем.
   */
  app_found: boolean;
}

export interface Detection {
  launchers: LauncherInfo[];
  /** Какие лаунчеры запущены сейчас (закрываются на время установки). */
  running: LauncherKind[];
}

export interface DeployPlan {
  zip_path: string;
  world_prefix: string;
  content_prefix: string;
  overrides_alias: boolean;
  world_name: string;
  layers: string[];
  deploy_mods: boolean;
  wipe_mods: boolean;
  /** Включённые пользователем ресурспаки (с галочек экрана проверки). */
  resource_packs: ResourcePackSelection[];
}

export interface DeployReport {
  world_dir_name: string;
  mods_written: string[];
  backed_up: string[];
  skipped_identical: string[];
  /** Финальные имена ресурспаков в resourcepacks/. */
  resource_packs_written: string[];
}

/** Итог подготовки версии TLauncher (ensure_tlauncher_version). */
export interface EnsureResult {
  target: GameTarget;
  /** Общие mods/ пусты — wipe не потребуется. */
  mods_empty: boolean;
  /** Предвыбор версии в конфиге TL выполнен. */
  preselected: boolean;
}

export interface StageEvent {
  stage: string;
  /** Байты текущей стадии (несжатые). `total_bytes === 0` — стадия без меры. */
  done_bytes: number;
  total_bytes: number;
  current_file: string;
  /** Сквозной прогресс всего деплоя в байтах (ставит бэкенд; у провижининга нет). */
  overall_done?: number | null;
  overall_total?: number | null;
}

/** Ошибка команды: `{code, message, details}` (код E_* для «Подробнее»). */
export interface CmdError {
  code: string;
  message: string;
  details: string;
}

export const LAUNCHER_NAMES: Record<LauncherKind, string> = {
  prism: 'Prism Launcher',
  pinecone: 'Pinecone Launcher',
  polymc: 'PolyMC',
  multimc: 'MultiMC',
  modrinth: 'Modrinth App',
  manual: 'Папка вручную',
  tlauncher: 'TLauncher',
  legacy: 'Legacy Launcher',
  official: 'Minecraft Launcher',
};

export const LOADER_NAMES: Record<LoaderKind, string> = {
  vanilla: 'Ванилла',
  fabric: 'Fabric',
  forge: 'Forge',
  quilt: 'Quilt',
  neoforge: 'NeoForge',
};
