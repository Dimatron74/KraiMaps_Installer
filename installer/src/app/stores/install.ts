/** Машина состояний визарда установки (Pinia). */
import { defineStore } from 'pinia';
import { listen } from '@tauri-apps/api/event';
import { api } from '../../shared/api/commands';
import { toErrorDetails, toUserMessage } from '../../shared/utils/errors';
import type {
  CmdError,
  DeployReport,
  Detection,
  EnsureResult,
  GameTarget,
  LauncherInfo,
  LauncherKind,
  LoaderKind,
  PackAnalysis,
  StageEvent,
} from '../../shared/types/installer';
import { LAUNCHER_NAMES, LOADER_NAMES } from '../../shared/types/installer';

export type Step =
  | 'file'
  | 'analyzing'
  | 'confirm'
  | 'launcher'
  | 'target'
  | 'wipe'
  | 'conflict'
  | 'deploying'
  | 'done'
  | 'error';

export interface InstallError extends CmdError {
  hint: string;
  /** Официальная страница, где решается проблема. Кнопка, а не текст в подсказке. */
  action?: { label: string; url: string };
}

/** Мета карты с сайта (install-manifest): влияет на версию загрузчика. */
export interface SiteMeta {
  loaderSlug: string | null;
  loaderVersion: string | null;
}

/**
 * Slug загрузчика сайта → LoaderKind. Строгий allowlist: неизвестное
 * (напр. legacy 'modded') — null, не гадаем.
 */
export function mapSiteLoaderSlug(slug: string | null | undefined): LoaderKind | null {
  if (!slug) return null;
  const s = slug.trim().toLowerCase();
  if (s === 'vanilla' || s === 'fabric' || s === 'forge' || s === 'quilt' || s === 'neoforge') {
    return s;
  }
  return null;
}

const MANUAL_ROOTS_KEY = 'mi.manualRoots';
const KNOWN_ROOTS_KEY = 'mi.knownRoots';

/**
 * Лаунчеры без инстансов: у них цели — версии, а не папки профилей, поэтому
 * новую цель готовит отдельная команда (`ensure_*`), а не `create_instance`.
 */
export const VERSION_LAUNCHERS: LauncherKind[] = ['tlauncher', 'legacy', 'official'];

function isVersionLauncher(kind: LauncherKind): boolean {
  return VERSION_LAUNCHERS.includes(kind);
}

/**
 * Точная версия загрузчика, на которую мы считаем.
 *
 * Резолв (`resolveLoaderVersion`) подставляет актуальную версию под версию
 * игры; если он не удался, остаётся подсказка из манифеста/архива, а если её
 * нет — `null`. `null` означает «не знаем», и всё, что зависит от точной версии
 * (список подходящих целей, кнопка подготовки версии), обязано в этом случае
 * **ничего не скрывать**: ложное «версия уже есть» хуже лишней кнопки.
 */
function effectiveLoaderVersion(state: {
  pack: PackAnalysis | null;
  loaderVersion: string | null;
  siteMeta: SiteMeta | null;
}): string | null {
  const pack = state.pack;
  if (!pack?.is_modded) return null;
  const hint = state.siteMeta?.loaderVersion?.trim() || null;
  return state.loaderVersion ?? hint ?? pack.loader_version_hint ?? null;
}

/** Ответ резолва версии загрузчика, пока он в полёте (см. `ensureLoaderVersion`). */
let loaderVersionRequest: Promise<string | null> | null = null;

/** Подсказка следующего шага по коду ошибки (человекочитаемо, без жаргона). */
function hintFor(code: string, running: LauncherKind[], launcher: LauncherKind | null): string {
  const name = launcher ? LAUNCHER_NAMES[launcher] : null;
  switch (code) {
    case 'E_NO_LEVEL_DAT':
      return 'В архиве нет мира. Проверьте, что это карта Minecraft, а не модпак или инструкция.';
    case 'E_MULTI_WORLD':
      return 'В архиве несколько миров. Оставьте один и попробуйте снова.';
    case 'E_ZIPSLIP':
    case 'E_LIMIT':
      return 'Архив выглядит подозрительно или слишком большой. Скачайте карту заново с проверенного источника.';
    case 'E_ZIP_CORRUPT':
    case 'E_ZIP_OPEN':
      return 'Файл повреждён или недокачан. Скачайте архив заново.';
    case 'E_UNKNOWN_LOADER':
      return 'Не удалось определить загрузчик модов. Поставьте карту в существующий инстанс с нужным загрузчиком.';
    case 'E_NO_LAUNCHER':
      return 'Укажите папку игры вручную — ванильную карту можно поставить и без лаунчера.';
    case 'E_MODRINTH_SCHEMA':
      return 'База Modrinth App изменилась после обновления. Попробуйте ещё раз или обратитесь в поддержку.';
    case 'E_NO_DATA_DIR':
      return name
        ? `${name} не найден или не настроен. Запустите его один раз и повторите установку.`
        : 'Не нашли папку данных лаунчера. Запустите его один раз и повторите установку.';
    case 'E_NO_JAVA':
      return 'Установите Java 17 или новее — она нужна только для подготовки версии игры, сама игра запускается на рантайме лаунчера.';
    case 'E_PROVISION_FAILED':
      return 'Не получилось подготовить версию игры. Проверьте интернет и попробуйте снова.';
    case 'E_HASH_MISMATCH':
      return 'Скачанный установщик не прошёл проверку. Попробуйте снова — качаем только с официальных серверов.';
    case 'E_LAUNCH_FAILED':
      if (name) {
        return `Не получилось открыть ${name}. Запустите его вручную.`;
      }
      return running.length > 0
        ? 'Не получилось открыть приложение. Запустите его вручную.'
        : 'Проверьте, что лаунчер на месте, и попробуйте ещё раз.';
    default:
      return 'Попробуйте ещё раз. Если ошибка повторяется — покажите код из «Подробнее» поддержке.';
  }
}

/**
 * Официальный источник Java. Eclipse Adoptium — сборки Temurin, на них
 * ссылается и сам Minecraft Launcher. Версию 17 берём из фильтра на
 * странице: так ссылка не протухнет при выходе новых LTS.
 */
const JAVA_DOWNLOAD_URL = 'https://adoptium.net/temurin/releases/?version=17';

/** Ошибки, где нужен не текст подсказки, а действие по официальной ссылке. */
function actionFor(code: string): InstallError['action'] {
  if (code === 'E_NO_JAVA') {
    return { label: 'Скачать Java 17', url: JAVA_DOWNLOAD_URL };
  }
  return undefined;
}

function toInstallError(
  e: unknown,
  running: LauncherKind[],
  launcher: LauncherKind | null,
): InstallError {
  const err = e as Partial<CmdError>;
  const code = typeof err.code === 'string' ? err.code : 'E_IO';
  return {
    code,
    message: toUserMessage(e),
    details: toErrorDetails(e),
    hint: hintFor(code, running, launcher),
    action: actionFor(code),
  };
}

export const useInstallStore = defineStore('install', {
  state: () => ({
    step: 'file' as Step,
    zipPath: '' as string,
    pack: null as PackAnalysis | null,
    detection: null as Detection | null,
    launcher: null as LauncherInfo | null,
    target: null as GameTarget | null,
    /** Цель только что создана (чистый деплой, wipe не нужен). */
    targetIsFresh: false,
    /** Точная версия загрузчика для нового инстанса (+ пометка авто). */
    loaderVersion: null as string | null,
    loaderVersionAuto: false,
    /** Мета install-manifest (только site-поток; ручной выбор файла — null). */
    siteMeta: null as SiteMeta | null,
    /** Архив пришёл со скачивания с сайта (лежит в нашем temp) — после успеха
     * его можно удалить. Ручной файл пользователя не трогаем никогда. */
    fromSiteDownload: false,
    /** Название карты с сайта для имени инстанса (sanitize — в Rust). */
    siteTitle: null as string | null,
    /** Локальный путь иконки инстанса (prepare_instance_icon, best-effort). */
    siteIconPath: null as string | null,
    /** Data URL иконки для превью на ConfirmStep. */
    siteIconUrl: null as string | null,
    /** Источники включённых ресурспаков (галочки экрана проверки). */
    enabledResourcePacks: [] as string[],
    /** Data URL иконок ресурспаков (pack.png): source → data URL. */
    resourcePackIcons: {} as Record<string, string>,
    /** Имя мира с учётом конфликта (выбор пользователя). */
    finalWorldName: '' as string,
    suggestedWorldName: '' as string,
    customWorldName: '' as string,
    progress: null as StageEvent | null,
    report: null as DeployReport | null,
    error: null as InstallError | null,
    errorReturnStep: 'file' as Step,
    busy: false,
    statusLine: '' as string,
    manualRoots: [] as string[],
    /** Ранее найденные exe/корни: портативка находится и без запуска. */
    knownRoots: [] as string[],
    /** Идёт ли деплой модов с wipe (для экрана wipe). */
    wipeConfirmed: false,
    /** Заметка про предвыбор версии для экрана Done. */
    versionNote: null as string | null,
    /**
     * Лаунчер работал на старте установки (замер до первого бэкенд-вызова —
     * бэкенд сам закрывает Modrinth/TLauncher при create/ensure и съел бы
     * сигнал). По флагу deploy() решает, открывать ли его обратно.
     */
    launcherWasRunning: false,
  }),

  getters: {
    /** Версии для фильтра: офлайн знаем только версию мира из level.dat. */
    filterVersions(state): string[] {
      const v = state.pack?.world_version;
      return v ? [v] : [];
    },

    suitableTargets(state): GameTarget[] {
      const pack = state.pack;
      const launcher = state.launcher;
      if (!pack || !launcher) return [];
      if (launcher.kind === 'manual') return pack.is_modded ? [] : launcher.targets;
      const loaderVersion = effectiveLoaderVersion(state);
      return launcher.targets.filter((t) => {
        if (pack.world_version && t.minecraft_version !== pack.world_version) return false;
        if (pack.is_modded) {
          if (t.loader !== pack.loader) return false;
          // Версия загрузчика решает не меньше типа: Fabric 0.16.9 не запустит
          // карту, собранную под 0.17.1.
          if (loaderVersion && (t.loader_version ?? null) !== loaderVersion) return false;
        }
        return true;
      });
    },

    /**
     * Версия, чью папку пришлось бы создавать заново, — если она уже есть.
     *
     * У Legacy и TLauncher версия это папка `versions/<Имя>/`, а папка на
     * комбинацию (версия игры, загрузчик, версия загрузчика) ровно одна:
     * «создать новый инстанс» там не создаёт ничего, а приводит в уже
     * существующую папку. Значит кнопку на месте такой версии показывать
     * незачем — правду пользователю говорит карточка в списке целей.
     *
     * Критерий тот же, что у бэкенд-`find_match`, поэтому расхождений между
     * «кнопка скрыта» и «установка создаст новую папку» не будет. Недокачанные
     * версии целями не считаются, так что сломанная версия не спрячет кнопку,
     * которая её чинит.
     */
    takenVersionTarget(state): GameTarget | null {
      const pack = state.pack;
      const launcher = state.launcher;
      if (!pack || !launcher) return null;
      // У официального лаунчера установок сколько угодно: папка игры выводится
      // из имени, поэтому кнопка «создать установку» создаёт новую всегда.
      if (!isVersionLauncher(launcher.kind) || launcher.kind === 'official') return null;
      const mc = pack.world_version;
      if (!mc) return null;
      const loaderVersion = effectiveLoaderVersion(state);
      return (
        launcher.targets.find(
          (t) =>
            t.minecraft_version === mc &&
            t.loader === pack.loader &&
            (t.loader_version ?? null) === loaderVersion,
        ) ?? null
      );
    },

    /** Подпись и пояснение кнопки «создать» для текущей пары лаунчер/карта. */
    newTargetCta(state): { title: string; hint: string } | null {
      const pack = state.pack;
      const launcher = state.launcher;
      if (!pack || !launcher || launcher.kind === 'manual') return null;
      const mc = pack.world_version ?? '';
      const mapName = state.siteTitle ?? pack.world_name;

      if (launcher.kind === 'official') {
        return {
          title: `Создать установку «${mapName} ${mc}»`,
          hint: 'Своя папка игры под карту — ваши установки и миры не пострадают. Таких установок можно сделать сколько угодно, поэтому занятое имя не мешает: получится DeepMine 1.21.11_v2.',
        };
      }
      if (isVersionLauncher(launcher.kind)) {
        if (!pack.is_modded) {
          return launcher.kind === 'tlauncher'
            ? {
                title: 'Установить в TLauncher',
                hint: 'У TLauncher одна папка игры для всех версий — карта встанет рядом с вашими мирами, а версию игры вы выберете в лаунчере.',
              }
            : {
                title: `Установить версию «${mc}»`,
                hint: 'Подготовим папку для миров и выберем версию в Legacy Launcher. Саму версию игры лаунчер скачает при первом запуске — это займёт несколько минут.',
              };
        }
        const loaderVersion = effectiveLoaderVersion(state);
        const version = `${mc} · ${LOADER_NAMES[pack.loader]}${loaderVersion ? ` ${loaderVersion}` : ''}`;
        return {
          title: `Подготовить «${version}»`,
          hint:
            launcher.kind === 'tlauncher'
              ? 'Скачаем официальный установщик и подготовим версию с пометкой [KK] — ничего нажимать не нужно. У TLauncher папка mods/ общая для всех версий, поэтому моды карты заменят текущие (старые сохраним в бэкап).'
              : 'Скачаем официальный установщик и подготовим версию с пометкой [KK] — ничего нажимать не нужно.',
        };
      }
      return {
        title: `Создать новый инстанс «${mapName}»`,
        hint: 'Рекомендуется для модовых карт — ничего чужого не тронем.',
      };
    },

    needsWipe(state): boolean {
      return !!(
        state.pack?.is_modded &&
        state.target &&
        !state.targetIsFresh &&
        state.target.launcher !== 'manual'
      );
    },

    /**
     * Расхождение «сайт vs архив» по загрузчику (только модовые паки;
     * ванилле загрузчик безразличен — D4). Тип всегда берём из архива,
     * текст — для честного предупреждения на ConfirmStep.
     */
    siteLoaderConflict(state): string | null {
      const pack = state.pack;
      const slug = state.siteMeta?.loaderSlug?.trim() || null;
      if (!pack?.is_modded || !slug) return null;
      const mapped = mapSiteLoaderSlug(slug);
      if (mapped === null) {
        return `Сайт указал неизвестный загрузчик «${slug}» — ставим по содержимому архива (${LOADER_NAMES[pack.loader]}).`;
      }
      if (mapped !== pack.loader) {
        return `Сайт указал ${LOADER_NAMES[mapped]}, в архиве найден ${LOADER_NAMES[pack.loader]} — ставим по содержимому архива.`;
      }
      return null;
    },
  },

  actions: {
    loadManualRoots() {
      try {
        this.manualRoots = JSON.parse(localStorage.getItem(MANUAL_ROOTS_KEY) ?? '[]');
        this.knownRoots = JSON.parse(localStorage.getItem(KNOWN_ROOTS_KEY) ?? '[]');
      } catch {
        this.manualRoots = [];
        this.knownRoots = [];
      }
    },

    saveManualRoot(path: string) {
      if (!this.manualRoots.includes(path)) {
        this.manualRoots.push(path);
        localStorage.setItem(MANUAL_ROOTS_KEY, JSON.stringify(this.manualRoots));
      }
    },

    /** Запомнить найденные exe/корни для будущих детектов без запуска. */
    rememberRoots() {
      const found = new Set(this.knownRoots);
      for (const l of this.detection?.launchers ?? []) {
        if (l.exe_path) found.add(l.exe_path);
        found.add(l.data_dir);
      }
      this.knownRoots = [...found];
      localStorage.setItem(KNOWN_ROOTS_KEY, JSON.stringify(this.knownRoots));
    },

    reset() {
      // Флоу брошен после закрытия лаунчера (экраны wipe/conflict → домой):
      // возвращаем его, иначе останется закрытым. Первой строкой — ниже
      // launcher уже занулен. Флаг сбросит сам экшен.
      void this.endInstallWork();
      this.step = 'file';
      this.zipPath = '';
      this.pack = null;
      this.siteMeta = null;
      this.fromSiteDownload = false;
      this.siteTitle = null;
      this.siteIconPath = null;
      this.siteIconUrl = null;
      this.enabledResourcePacks = [];
      this.resourcePackIcons = {};
      this.detection = null;
      this.launcher = null;
      this.target = null;
      this.targetIsFresh = false;
      this.loaderVersion = null;
      this.loaderVersionAuto = false;
      this.finalWorldName = '';
      this.suggestedWorldName = '';
      this.customWorldName = '';
      this.progress = null;
      this.report = null;
      this.error = null;
      this.busy = false;
      this.statusLine = '';
      this.wipeConfirmed = false;
      this.versionNote = null;
    },

    fail(e: unknown, returnStep: Step) {
      this.error = toInstallError(e, this.detection?.running ?? [], this.launcher?.kind ?? null);
      this.errorReturnStep = returnStep;
      this.step = 'error';
      this.busy = false;
      this.statusLine = '';
    },

    async pickZip(path: string) {
      this.reset();
      this.zipPath = path;
      this.step = 'analyzing';
      try {
        this.pack = await api.analyzePack(path);
        this.finalWorldName = this.pack.world_name;
        this.enabledResourcePacks = (this.pack.resource_packs ?? []).map((p) => p.source);
        await this.prepareIcon();
        await this.prepareResourcePackIcons();
        this.step = 'confirm';
      } catch (e) {
        this.fail(e, 'file');
      }
    },

    /**
     * Иконка из архива для инстанса и превью на ConfirmStep.
     * Общее для обоих потоков (файл вручную / с сайта), best-effort.
     */
    async prepareIcon() {
      const pack = this.pack;
      if (!pack) {
        this.siteIconPath = null;
        this.siteIconUrl = null;
        return;
      }
      try {
        const p = await api.prepareInstanceIcon(
          this.zipPath,
          pack.content_prefix,
          pack.world_prefix,
        );
        this.siteIconPath = p;
        this.siteIconUrl = p ? await api.iconDataUrl(p).catch(() => null) : null;
      } catch {
        this.siteIconPath = null;
        this.siteIconUrl = null;
      }
    },

    /**
     * Иконки ресурспаков (pack.png) для экрана проверки. Best-effort:
     * неуспех по одному паку остальных не касается.
     */
    async prepareResourcePackIcons() {
      const pack = this.pack;
      if (!pack) return;
      const icons: Record<string, string> = {};
      await Promise.all(
        (pack.resource_packs ?? []).map(async (rp) => {
          try {
            const url = await api.resourcePackIcon(this.zipPath, rp.source, rp.kind);
            if (url) icons[rp.source] = url;
          } catch {
            // Молча пропускаем — пак встанет и без превью.
          }
        }),
      );
      this.resourcePackIcons = icons;
    },

    /** Галочка ресурспака на экране проверки (снять — не ставить этот пак). */
    toggleResourcePack(source: string) {
      const i = this.enabledResourcePacks.indexOf(source);
      if (i >= 0) {
        this.enabledResourcePacks.splice(i, 1);
      } else {
        this.enabledResourcePacks.push(source);
      }
    },

    /** Включённые паки для плана деплоя. */
    selectedResourcePacks() {
      const enabled = new Set(this.enabledResourcePacks);
      return (this.pack?.resource_packs ?? [])
        .filter((p) => enabled.has(p.source))
        .map((p) => ({ source: p.source, name: p.name, kind: p.kind }));
    },

    /**
     * Мета site-потока после pickZip (pickZip делает reset — вызывать строго
     * после него; ручной выбор файла siteMeta не ставит).
     */
    setSiteMeta(meta: SiteMeta | null, title: string | null) {
      this.siteMeta = meta;
      this.siteTitle = title?.trim() ? title.trim() : null;
      // site-поток всегда идёт через скачивание в наш temp — архив временный.
      this.fromSiteDownload = true;
    },

    /**
     * Временные файлы завершённой установки за собой не оставляем: архив
     * (только site-поток — ручной файл пользователя свят) и временную
     * иконку (в инстанс уже легла копия при создании). Best-effort:
     * успех установки от чистки не зависит, бэкенд чужие/внешние пути
     * отклоняет сам.
     */
    discardTempLeftovers() {
      const paths: string[] = [];
      if (this.fromSiteDownload && this.zipPath) paths.push(this.zipPath);
      if (this.siteIconPath) paths.push(this.siteIconPath);
      if (paths.length === 0) return;
      void api.discardTempFiles(paths).catch(() => {});
    },

    async confirmPack() {
      this.busy = true;
      this.statusLine = 'Ищем лаунчеры…';
      try {
        this.detection = await api.detectLaunchers(this.manualRoots, this.knownRoots);
        this.rememberRoots();
        // Запоминаем только валидные ручные корни (детект их принял).
        for (const l of this.detection.launchers) {
          if (l.kind === 'manual') this.saveManualRoot(l.data_dir);
        }
        const launchers = this.detection.launchers.filter((l) =>
          this.pack?.is_modded ? l.kind !== 'manual' : true,
        );
        if (launchers.length === 0) {
          throw { code: 'E_NO_LAUNCHER', message: 'Лаунчеры не найдены', details: 'no launchers' };
        }
        if (launchers.length === 1) {
          this.selectLauncher(launchers[0]!);
        } else {
          this.busy = false;
          this.statusLine = '';
          this.step = 'launcher';
        }
      } catch (e) {
        this.fail(e, 'confirm');
      }
    },

    /** Повторный поиск без возврата назад: запущенный лаунчер находится всегда. */
    async rescanLaunchers() {
      this.busy = true;
      this.statusLine = 'Ищем лаунчеры…';
      try {
        this.detection = await api.detectLaunchers(this.manualRoots, this.knownRoots);
        this.rememberRoots();
        const launchers = this.detection.launchers.filter((l) =>
          this.pack?.is_modded ? l.kind !== 'manual' : true,
        );
        if (launchers.length === 0) {
          throw { code: 'E_NO_LAUNCHER', message: 'Лаунчеры не найдены', details: 'rescan: none' };
        }
        if (launchers.length === 1) {
          this.selectLauncher(launchers[0]!);
        } else {
          this.busy = false;
          this.statusLine = '';
        }
      } catch (e) {
        this.fail(e, 'launcher');
      }
    },

    selectLauncher(l: LauncherInfo) {
      this.launcher = l;
      this.busy = false;
      this.statusLine = '';
      // Экран цели показывает кнопку «подготовить версию» и подходящие версии,
      // а обе вещи зависят от точной версии загрузчика — резолвим её заранее.
      // Провал здесь не фатален: `createVersionTarget` повторит резолв и
      // покажет ту же ошибку, что и раньше.
      void this.ensureLoaderVersion().catch(() => {});
      const suitable = this.suitableTargets;
      if (suitable.length === 0) {
        // Не во что ставить — молча создаём новый (статус в прогрессе).
        void this.createNewTarget();
      } else {
        this.step = 'target';
      }
    },

    /**
     * Точная версия загрузчика, резолвится один раз: результат нужен и экрану
     * цели, и подготовке версии. Хранится в `loaderVersion` — там же живёт
     * отметка «подобрана автоматически».
     */
    async ensureLoaderVersion(): Promise<string | null> {
      const pack = this.pack;
      if (!pack?.is_modded) return null;
      const mc = pack.world_version;
      if (!mc) return null;
      if (this.loaderVersion) return this.loaderVersion;
      // Запрос в полёте переиспользуем: выбрать лаунчер с авто-созданием цели
      // и нажать кнопку можно не дожидаясь сети, а второй запрос вернул бы то
      // же самое. Сбрасываем в любом исходе — после провала клик должен суметь
      // повторить.
      loaderVersionRequest ??= (async () => {
        // Приоритет по спеке (D7): точная версия автора из манифеста проходит
        // как есть (без сети); диапазон/отсутствие — хинт архива → meta-API.
        // Хинт из архива может быть диапазоном ("[40,)") — бэкенд такой не
        // примет и отрезолвит в последнюю версию.
        const siteVersion = this.siteMeta?.loaderVersion?.trim() || null;
        const baseHint = siteVersion ?? pack.loader_version_hint;
        this.loaderVersion = await api.resolveLoaderVersion(pack.loader, mc, baseHint);
        this.loaderVersionAuto = this.loaderVersion !== baseHint;
        return this.loaderVersion;
      })().finally(() => {
        loaderVersionRequest = null;
      });
      return loaderVersionRequest;
    },

    /**
     * Закрывает запущенный лаунчер до первого бэкенд-вызова и запоминает,
     * работал ли он (OR-накопление: путь через create и путь через deploy
     * дополняют друг друга, а не затирают). Ручной папке закрывать нечего.
     */
    async beginInstallWork() {
      const launcher = this.launcher;
      if (!launcher || launcher.kind === 'manual') return;
      try {
        this.launcherWasRunning =
          (await api.closeLauncher(launcher.kind)) || this.launcherWasRunning;
      } catch {
        // Не-закрытие — не повод останавливать установку.
      }
    },

    /**
     * Возвращает лаунчер, если мы его закрывали (и на успехе, и на провале).
     * Молча: установка уже завершена, шум ни к чему. Флаг сбрасываем сразу,
     * чтобы повторный вызов не открыл его дважды.
     */
    async endInstallWork() {
      const launcher = this.launcher;
      if (!launcher || !this.launcherWasRunning) return;
      this.launcherWasRunning = false;
      await api.reopenLauncher(launcher.kind, launcher.exe_path).catch(() => {});
    },

    async createNewTarget() {
      const pack = this.pack;
      const launcher = this.launcher;
      if (!pack || !launcher || launcher.kind === 'manual') return;
      // У TLauncher, Legacy Launcher и официального лаунчера цели — версии,
      // а не инстансы: их готовит отдельная команда, `create_instance` не
      // вызывается.
      if (isVersionLauncher(launcher.kind)) {
        await this.createVersionTarget();
        return;
      }
      const mc = pack.world_version;
      if (!mc) {
        this.fail(
          {
            code: 'E_UNKNOWN_LOADER',
            message: 'Неизвестна версия игры',
            details: 'world_version is null',
          },
          'target',
        );
        return;
      }
      // Закрываем после мгновенных проверок, до первого бэкенд-вызова:
      // бэкенд сам закрывает Modrinth/TLauncher при create/ensure и съел бы
      // сигнал «был запущен».
      await this.beginInstallWork();
      const wasStep = this.step;
      this.busy = true;
      this.statusLine = `Создаём инстанс «${this.siteTitle ?? pack.world_name}»…`;
      this.step = 'deploying';
      this.progress = null;
      try {
        let loaderVersion: string | null = null;
        let auto = false;
        if (pack.is_modded) {
          // Приоритет версии по спеке (D7): точная версия автора из манифеста
          // проходит через резолв как есть (без сети); диапазон/отсутствие —
          // хинт архива → meta-API. Тип загрузчика всегда из архива.
          const siteVersion = this.siteMeta?.loaderVersion?.trim() || null;
          // Хинт из архива может быть диапазоном ("[40,)") — его нельзя
          // писать в инстанс. Бэкенд пропустит точную версию как есть,
          // а диапазон/отсутствие — отрезолвит через meta-API в последнюю.
          loaderVersion = await api.resolveLoaderVersion(
            pack.loader,
            mc,
            siteVersion ?? pack.loader_version_hint,
          );
          const baseHint = siteVersion ?? pack.loader_version_hint;
          auto = loaderVersion !== baseHint;
        }
        this.loaderVersion = loaderVersion;
        this.loaderVersionAuto = auto;
        // Имя инстанса — название карты с сайта (sanitize — в Rust);
        // без сайта — имя мира из архива.
        const displayName = this.siteTitle ?? pack.world_name;
        this.target = await api.createInstance({
          launcher: launcher.kind,
          dataDir: launcher.data_dir,
          name: displayName,
          mcVersion: mc,
          loader: pack.loader,
          loaderVersion,
          iconPath: this.siteIconPath,
        });
        this.targetIsFresh = true;
        // Обновляем детект, чтобы новый инстанс был виден везде.
        this.detection = await api.detectLaunchers(this.manualRoots, this.knownRoots);
        this.rememberRoots();
        // Привязка строго по папке данных: однотипных лаунчеров может быть несколько.
        this.launcher =
          this.detection.launchers.find(
            (l) => l.kind === launcher.kind && l.data_dir === launcher.data_dir,
          ) ?? launcher;
        await this.proceedAfterTarget();
      } catch (e) {
        await this.endInstallWork();
        this.fail(e, wasStep === 'deploying' ? 'target' : wasStep);
      }
    },

    selectTarget(t: GameTarget) {
      this.target = t;
      this.targetIsFresh = false;
      void this.proceedAfterTarget();
    },

    /**
     * Маршрутизация после выбора цели: модовая карта в несвежую цель —
     * сначала экран wipe (иначе моды смерджатся молча), иначе дальше.
     * Единая точка для всех путей (выбор, создание, провижининг TL).
     */
    async proceedAfterTarget() {
      if (this.needsWipe) {
        this.busy = false;
        this.statusLine = '';
        this.step = 'wipe';
      } else {
        await this.afterTargetChosen();
      }
    },

    /**
     * Цель для лаунчера «без инстансов» — TLauncher, Legacy Launcher или
     * официальный Minecraft Launcher.
     *
     * Официальный лаунчер: создаём СВОЮ установку с отдельной папкой игры
     * даже для ванильной карты — в этом его смысл, и чужие установки
     * пользователя при этом не трогаем.
     *
     * TLauncher и Legacy: у них одна папка игры на все версии. Ванилле
     * версия не нужна — синтетическая цель сразу в деплой. Модовой карте
     * версию готовит официальный установщик загрузчика: поиск → провижининг →
     * пометка [KK] → предвыбор. Общие `mods/` никогда не «свежие»: свежесть
     * ставим только если папка реально пуста, иначе дальше wipe-экран.
     */
    async createVersionTarget() {
      const pack = this.pack;
      const launcher = this.launcher;
      if (!pack || !launcher || !isVersionLauncher(launcher.kind)) return;
      const kind = launcher.kind;
      const wasStep = this.step;
      this.busy = true;
      this.progress = null;
      this.versionNote = null;

      const mc = pack.world_version;
      // TLauncher — единственный из «без инстансов», кому ванилле версия не
      // нужна: у него одна папка игры на все версии. Legacy раскладывает
      // saves по подпапкам режима, поэтому даже ванильной карте нужна
      // подготовка — указать папку `home/<версия>` и предвыбрать версию.
      // Официальному лаунчеру нужна своя установка.
      const needsVersion = pack.is_modded || kind === 'official' || kind === 'legacy';
      if (!needsVersion) {
        this.statusLine = 'Готовим цель…';
        this.step = 'deploying';
        this.target = {
          launcher: kind,
          name: LAUNCHER_NAMES[kind],
          path: launcher.data_dir,
          minecraft_version: mc ?? '',
          loader: 'vanilla',
          loader_version: null,
        };
        this.targetIsFresh = true;
        await this.proceedAfterTarget();
        return;
      }
      if (!mc) {
        this.fail(
          {
            code: 'E_UNKNOWN_LOADER',
            message: 'Неизвестна версия игры',
            details: 'world_version is null',
          },
          'target',
        );
        return;
      }
      // Ванилла выше ушла без бэкенд-вызовов (закроем в deploy),
      // проверка версии мгновенная — закрываем только перед работой.
      await this.beginInstallWork();
      this.statusLine = 'Готовим версию игры…';
      this.step = 'deploying';
      const unlisten = await listen<StageEvent>('provision-progress', (event) => {
        this.progress = event.payload;
      });
      try {
        const loaderVersion = await this.ensureLoaderVersion();
        this.statusLine = pack.is_modded
          ? 'Ставим версию игры — это займёт несколько минут…'
          : 'Готовим установку…';

        let res: EnsureResult;
        if (kind === 'official') {
          // `installDir` официального лаунчера — это его корень данных;
          // exe лежит в другом месте, поэтому передаём data_dir.
          res = await api.ensureOfficialInstallation({
            dataDir: launcher.data_dir,
            name: `${this.siteTitle ?? pack.world_name} ${mc}`,
            mcVersion: mc,
            loader: pack.loader,
            loaderVersion,
            iconPath: this.siteIconPath,
          });
        } else if (kind === 'legacy') {
          res = await api.ensureLegacyVersion({
            installDir: launcher.exe_path ?? launcher.data_dir,
            mcVersion: mc,
            loader: pack.loader,
            loaderVersion,
          });
        } else {
          res = await api.ensureTlauncherVersion({
            dataDir: launcher.data_dir,
            mcVersion: mc,
            loader: pack.loader,
            loaderVersion,
          });
        }
        this.target = res.target;
        // Общие mods/ свежие, только если реально пусты.
        this.targetIsFresh = res.mods_empty;
        if (!res.preselected) {
          this.versionNote = `${LAUNCHER_NAMES[kind]} не удалось закрыть — выберите версию «${res.target.name}» в лаунчере вручную.`;
        }
        // Обновляем детект, чтобы подготовленная версия была видна везде.
        this.detection = await api.detectLaunchers(this.manualRoots, this.knownRoots);
        this.rememberRoots();
        this.launcher =
          this.detection.launchers.find(
            (l) => l.kind === kind && l.data_dir === launcher.data_dir,
          ) ?? launcher;
        await this.proceedAfterTarget();
      } catch (e) {
        await this.endInstallWork();
        this.fail(e, wasStep === 'deploying' ? 'target' : wasStep);
      } finally {
        unlisten();
      }
    },

    confirmWipe() {
      this.wipeConfirmed = true;
      void this.afterTargetChosen();
    },

    async afterTargetChosen() {
      const target = this.target;
      const pack = this.pack;
      if (!target || !pack) return;
      this.busy = true;
      try {
        const suggested = await api.suggestWorldName(
          target,
          this.finalWorldName || pack.world_name,
        );
        this.suggestedWorldName = suggested;
        this.busy = false;
        if (suggested !== (this.finalWorldName || pack.world_name)) {
          this.customWorldName = suggested;
          this.step = 'conflict';
        } else {
          this.finalWorldName = suggested;
          await this.deploy();
        }
      } catch (e) {
        this.fail(e, 'target');
      }
    },

    async confirmConflictName() {
      const target = this.target;
      const name = this.customWorldName.trim();
      if (!target || !name) return;
      this.busy = true;
      try {
        const suggested = await api.suggestWorldName(target, name);
        if (suggested !== name) {
          this.customWorldName = suggested;
          this.suggestedWorldName = suggested;
          this.busy = false;
          return;
        }
        this.finalWorldName = name;
        await this.deploy();
      } catch (e) {
        this.fail(e, 'conflict');
      }
    },

    useSuggestedName() {
      this.finalWorldName = this.suggestedWorldName;
      void this.deploy();
    },

    async deploy() {
      const target = this.target;
      const pack = this.pack;
      if (!target || !pack) return;
      this.step = 'deploying';
      this.busy = true;
      this.statusLine = 'Устанавливаем…';
      this.progress = null;
      // Путь через существующую цель (без create): замеряем здесь.
      // Путь через create уже замерен раньше — OR-накопление в экшене.
      await this.beginInstallWork();
      const unlisten = await listen<StageEvent>('deploy-progress', (event) => {
        this.progress = event.payload;
      });
      try {
        this.report = await api.deployPack(target, {
          zip_path: this.zipPath,
          world_prefix: pack.world_prefix,
          content_prefix: pack.content_prefix,
          overrides_alias: pack.overrides_alias,
          world_name: this.finalWorldName || pack.world_name,
          layers: pack.layers,
          deploy_mods: pack.is_modded,
          wipe_mods: this.wipeConfirmed,
          resource_packs: this.selectedResourcePacks(),
        });
        this.busy = false;
        this.statusLine = '';
        this.step = 'done';
        this.discardTempLeftovers();
        await this.registerQuickPlay();
      } catch (e) {
        this.fail(e, 'target');
      } finally {
        unlisten();
        // Reopen — и на успехе, и на провале: раз закрыли, надо вернуть.
        await this.endInstallWork();
      }
    },

    /**
     * У официального лаунчера «Играть» работает через панель Quick Play:
     * прописываем мир в `launcher_quick_play.json`, и клик по иконке открывает
     * игру сразу в нём. Best-effort — установка уже успешна, подсказывать
     * пользователю ничего не нужно, максимум — тихо пропускаем.
     */
    async registerQuickPlay() {
      const target = this.target;
      const report = this.report;
      const launcher = this.launcher;
      if (!target || !report || !launcher || launcher.kind !== 'official') return;
      try {
        const enabled = await api.addOfficialQuickPlay({
          dataDir: launcher.data_dir,
          target,
          worldDirName: report.world_dir_name,
          worldName: this.finalWorldName || report.world_dir_name,
          iconPath: this.siteIconPath,
        });
        if (!enabled) {
          this.versionNote =
            'Чтобы открыть мир одним кликом, включите «Быстрый запуск» (Quick Play) в настройках Minecraft Launcher.';
        }
      } catch {
        // Установка уже успешна: панель быстрого запуска — необязательный
        // бонус, молча пропускаем.
      }
    },

    async play() {
      const target = this.target;
      const launcher = this.launcher;
      if (!target || !launcher || !this.report) return;
      this.busy = true;
      try {
        const exe = launcher.exe_path;
        if (
          target.launcher === 'prism' ||
          target.launcher === 'pinecone' ||
          target.launcher === 'polymc' ||
          target.launcher === 'multimc'
        ) {
          await api.launchGame({
            launcher: target.launcher,
            exe,
            instanceId: target.path,
            world: this.report.world_dir_name,
            folder: null,
          });
        } else if (target.launcher === 'modrinth') {
          await api.launchGame({
            launcher: 'modrinth',
            exe,
            instanceId: target.path,
            world: null,
            folder: null,
          });
        } else if (isVersionLauncher(target.launcher)) {
          // Для TLauncher/Legacy `instanceId` — имя версии (её выбирает CLI
          // лаунчера), для официального — что угодно: запускается приложение.
          await api.launchGame({
            launcher: target.launcher,
            exe,
            instanceId: target.name,
            world: null,
            folder: null,
          });
        } else {
          await api.launchGame({
            launcher: 'manual',
            exe: null,
            instanceId: target.path,
            world: null,
            folder: target.path,
          });
        }
      } catch (e) {
        this.fail(e, 'done');
        return;
      }
      this.busy = false;
    },
  },
});
