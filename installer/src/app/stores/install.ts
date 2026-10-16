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
  LoaderKind,
  PackAnalysis,
  StageEvent,
} from '../../shared/types/installer';
import { LOADER_NAMES } from '../../shared/types/installer';

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

/** Подсказка следующего шага по коду ошибки (человекочитаемо, без жаргона). */
function hintFor(code: string, modrinthRunning: boolean, tlauncherRunning: boolean): string {
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
    case 'E_NO_JAVA':
      return 'Не нашли Java 17+ для подготовки версии игры. Запустите TLauncher один раз — он докачает нужный рантайм сам.';
    case 'E_PROVISION_FAILED':
      return 'Не получилось подготовить версию игры. Проверьте интернет и попробуйте снова.';
    case 'E_HASH_MISMATCH':
      return 'Скачанный установщик не прошёл проверку. Попробуйте снова — качаем только с официальных серверов.';
    case 'E_LAUNCH_FAILED':
      if (tlauncherRunning) {
        return 'Не получилось открыть TLauncher. Запустите его вручную.';
      }
      return modrinthRunning
        ? 'Не получилось открыть приложение. Запустите его вручную.'
        : 'Проверьте, что лаунчер на месте, и попробуйте ещё раз.';
    default:
      return 'Попробуйте ещё раз. Если ошибка повторяется — покажите код из «Подробнее» поддержке.';
  }
}

function toInstallError(e: unknown, modrinthRunning: boolean, tlauncherRunning: boolean): InstallError {
  const err = e as Partial<CmdError>;
  const code = typeof err.code === 'string' ? err.code : 'E_IO';
  return {
    code,
    message: toUserMessage(e),
    details: toErrorDetails(e),
    hint: hintFor(code, modrinthRunning, tlauncherRunning),
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
    /** Заметка про TLauncher для экрана Done (предвыбор версии пропущен). */
    tlauncherNote: null as string | null,
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
      return launcher.targets.filter((t) => {
        if (pack.world_version && t.minecraft_version !== pack.world_version) return false;
        if (pack.is_modded && t.loader !== pack.loader) return false;
        return true;
      });
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
      this.tlauncherNote = null;
    },

    fail(e: unknown, returnStep: Step) {
      this.error = toInstallError(
        e,
        this.detection?.modrinth_running ?? false,
        this.detection?.tlauncher_running ?? false,
      );
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

    selectLauncher(l: LauncherInfo) {      this.launcher = l;
      this.busy = false;
      this.statusLine = '';
      const suitable = this.suitableTargets;
      if (suitable.length === 0) {
        // Не во что ставить — молча создаём новый (статус в прогрессе).
        void this.createNewTarget();
      } else {
        this.step = 'target';
      }
    },

    async createNewTarget() {
      const pack = this.pack;
      const launcher = this.launcher;
      if (!pack || !launcher || launcher.kind === 'manual') return;
      // У TLauncher нет инстансов: вместо создания — подготовка версии.
      if (launcher.kind === 'tlauncher') {
        await this.createTlauncherTarget();
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
     * Цель для TLauncher: инстансов нет, общий game dir.
     * Ванилла — синтетическая цель сразу в деплой (версии не нужны).
     * Модовая — подготовка версии официальным установщиком
     * (ensure_tlauncher_version): поиск → провижининг → пометка [KK] →
     * предвыбор. Общие mods/ никогда не «свежие»: targetIsFresh ставим
     * только если mods/ реально пуста, иначе дальше wipe-экран.
     */
    async createTlauncherTarget() {
      const pack = this.pack;
      const launcher = this.launcher;
      if (!pack || !launcher || launcher.kind !== 'tlauncher') return;
      const wasStep = this.step;
      this.busy = true;
      this.progress = null;
      this.tlauncherNote = null;
      if (!pack.is_modded) {
        this.statusLine = 'Готовим цель…';
        this.step = 'deploying';
        this.target = {
          launcher: 'tlauncher',
          name: 'TLauncher',
          path: launcher.data_dir,
          minecraft_version: pack.world_version ?? '',
          loader: 'vanilla',
          loader_version: null,
        };
        this.targetIsFresh = true;
        await this.proceedAfterTarget();
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
      this.statusLine = 'Готовим версию игры…';
      this.step = 'deploying';
      const unlisten = await listen<StageEvent>('provision-progress', (event) => {
        this.progress = event.payload;
      });
      try {
        const siteVersion = this.siteMeta?.loaderVersion?.trim() || null;
        const loaderVersion = await api.resolveLoaderVersion(
          pack.loader,
          mc,
          siteVersion ?? pack.loader_version_hint,
        );
        const baseHint = siteVersion ?? pack.loader_version_hint;
        this.loaderVersion = loaderVersion;
        this.loaderVersionAuto = loaderVersion !== baseHint;
        this.statusLine = 'Ставим версию игры — это займёт несколько минут…';
        const res: EnsureResult = await api.ensureTlauncherVersion({
          dataDir: launcher.data_dir,
          mcVersion: mc,
          loader: pack.loader,
          loaderVersion,
        });
        this.target = res.target;
        // Общие mods/ свежие, только если реально пусты.
        this.targetIsFresh = res.mods_empty;
        if (!res.preselected) {
          this.tlauncherNote = `TLauncher не удалось закрыть — выберите версию «${res.target.name}» в лаунчере вручную.`;
        }
        // Обновляем детект, чтобы подготовленная версия была видна везде.
        this.detection = await api.detectLaunchers(this.manualRoots, this.knownRoots);
        this.rememberRoots();
        this.launcher =
          this.detection.launchers.find(
            (l) => l.kind === 'tlauncher' && l.data_dir === launcher.data_dir,
          ) ?? launcher;
        await this.proceedAfterTarget();
      } catch (e) {
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
        const suggested = await api.suggestWorldName(target, this.finalWorldName || pack.world_name);
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
      } catch (e) {
        this.fail(e, 'target');
      } finally {
        unlisten();
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
        } else if (target.launcher === 'tlauncher') {
          await api.launchGame({
            launcher: 'tlauncher',
            exe,
            instanceId: target.path,
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
