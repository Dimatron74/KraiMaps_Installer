/** Машина состояний визарда установки (Pinia). */
import { defineStore } from 'pinia';
import { listen } from '@tauri-apps/api/event';
import { api } from '../../shared/api/commands';
import { toErrorCode, toErrorDetails, toUserMessage } from '../../shared/utils/errors';
import type {
  CmdError,
  CompatFailure,
  CompatKind,
  CompatSummary,
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
import { compareMcVersions, isKnownMcVersion } from '../../shared/utils/version';

export type Step =
  | 'file'
  | 'analyzing'
  | 'confirm'
  | 'launcher'
  | 'target'
  | 'compat'
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
  /** Основная версия MC из манифеста: нужна, когда в архиве её нет. */
  minecraftPrimary: string | null;
}

/**
 * Маркер требования OptiFine в `loader.version`. Это не версия загрузчика:
 * в резолв версии он попадать не должен (см. `services/versions.rs`).
 */
const OPTIFINE_MARKER = 'optifine';

/** Версия загрузчика из манифеста; маркер OptiFine версией не считается. */
function siteLoaderVersionHint(meta: SiteMeta | null): string | null {
  const v = meta?.loaderVersion?.trim();
  if (!v || v.toLowerCase() === OPTIFINE_MARKER) return null;
  return v;
}

/** Манифест сайта требует OptiFine: маркер живёт в `loader.version`. */
function hasOptifineMarker(meta: SiteMeta | null): boolean {
  return meta?.loaderVersion?.trim().toLowerCase() === OPTIFINE_MARKER;
}

/**
 * Версия MC, под которую ставится слой: сначала мир из архива, затем
 * основная версия манифеста. Неизвестная версия — `null`, гадать запрещено.
 */
function targetMcVersionFor(pack: PackAnalysis | null, meta: SiteMeta | null): string | null {
  const indexed = pack?.mrpack?.mc_version?.trim();
  if (indexed) return indexed;
  const world = pack?.world_version?.trim();
  if (world) return world;
  const primary = meta?.minecraftPrimary?.trim();
  return primary || null;
}

/** Ветка слоя: Fabulously Optimized с 1.16.5, Forge + OptiFine до неё. */
function compatBranchFor(mc: string | null): CompatKind | null {
  if (!isKnownMcVersion(mc)) return null;
  const cmp = compareMcVersions(mc, '1.16.5');
  if (cmp === null) return null;
  return cmp >= 0 ? 'fo' : 'optifine';
}

/** Короткая подпись метода для CTA новой цели. */
function compatPlanLabelFor(compat: CompatSummary | null): string {
  if (!compat) return '';
  return compat.kind === 'optifine'
    ? `Forge ${compat.loader_version} + OptiFine`
    : 'Fabric + Fabulously Optimized';
}

/** Метод слоя предложением — для экрана проверки до резолва сборки. */
function compatMethodFor(kind: CompatKind | null): string {
  if (kind === 'fo') {
    return 'Поставим Fabric-сборку Fabulously Optimized — она заменяет OptiFine';
  }
  if (kind === 'optifine') {
    return 'Поставим Forge и OptiFine — официальный jar с optifine.net';
  }
  return '';
}

/**
 * Причина, по которой слой не поставится. Для OptiFine-ветки добавляем
 * VPN-подсказку: optifine.net в России часто недоступен без него.
 */
function compatUnavailableText(e: unknown, branch: CompatKind | null): string {
  const code = toErrorCode(e);
  const base =
    code === 'E_COMPAT_UNAVAILABLE' || code === 'E_COMPAT_RESOLVE'
      ? 'Сборку совместимости не удалось подобрать.'
      : 'Сборку совместимости не удалось подготовить.';
  return branch === 'optifine'
    ? `${base} Для optifine.net в России может понадобиться IP, отличный от РФ.`
    : base;
}

/** Ошибка слоя в том же виде, что приходит из `deploy_pack`/`apply_compat`. */
function toCompatFailure(e: unknown): CompatFailure {
  return {
    code: toErrorCode(e),
    message: toUserMessage(e),
    details: toErrorDetails(e),
  };
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
  const hint = pack.is_mrpack
    ? (pack.mrpack?.loader_version ?? pack.loader_version_hint)
    : siteLoaderVersionHint(state.siteMeta);
  return state.loaderVersion ?? hint ?? pack.loader_version_hint ?? null;
}

/** Ответ резолва версии загрузчика, пока он в полёте (см. `ensureLoaderVersion`). */
let loaderVersionRequest: Promise<string | null> | null = null;

/** Резолв слоя совместимости в полёте (см. `prepareCompat`). */
let compatRequest: Promise<void> | null = null;

// Новый флоу ждёт возврата лаунчера, даже если reset() застал незавершённое закрытие.
let launcherCloseRequest: Promise<boolean> | null = null;
let launcherReturnRequest: Promise<void> | null = null;

/**
 * Поколение флоу: `reset()` увеличивает его, и результат резолва, начатого
 * до сброса, не должен попасть в новое состояние (иначе всплывёт чужой
 * план слоя, а его temp-файл останется висеть).
 */
let compatGeneration = 0;

/** Сколько строк живого лога установщика держим в памяти. */
const PROVISION_LOG_MAX = 500;

/** Отмена приходит как обычная ошибка команды — отличаем её по коду. */
function isCancelError(e: unknown): boolean {
  return (e as Partial<CmdError> | null)?.code === 'E_CANCELLED';
}

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
      return 'Похоже у вас нет установленного поддерживаемого лаунчера. Скачайте PrismLauncher или любой другой и попробуйте снова.';
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
      return 'Скачанный установщик не прошёл проверку. Попробуйте снова, мы качаем только с официальных серверов.';
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
    /** Пустота mods/ не доказывает происхождение цели (§7.6 дизайна слоя). */
    targetIsFresh: false,
    targetModsEmpty: false,
    /** Точная версия загрузчика для нового инстанса (+ пометка авто). */
    loaderVersion: null as string | null,
    loaderVersionAuto: false,
    /** Слой совместимости (OptiFine): сводка или `null`, если не нужен. */
    compat: null as CompatSummary | null,
    /** Почему слой не будет поставлен (non-fatal), `null` — проблемы нет. */
    compatUnavailable: null as string | null,
    compatPreparationFailure: null as CompatFailure | null,
    compatPrepared: false,
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
    /** Живой лог установки загрузчика для раскрываемого «подробнее». */
    provisionLog: [] as string[],
    /** Отмена нажата: ждём `E_CANCELLED` из висящего `ensure_*`. */
    cancelling: false,
    /** Тихое уведомление после отмены (показывается на экране лаунчера). */
    cancelledNotice: null as string | null,
    progress: null as StageEvent | null,
    report: null as DeployReport | null,
    error: null as InstallError | null,
    errorReturnStep: 'file' as Step,
    busy: false,
    statusLine: '' as string,
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
    /**
     * Манифест сайта требует OptiFine. Маркер — не версия загрузчика, поэтому
     * сам по себе он ни на что, кроме слоя совместимости, не влияет.
     */
    requiresOptifine(state): boolean {
      return !!state.pack && !state.pack.is_mrpack && hasOptifineMarker(state.siteMeta);
    },

    /** Версия MC, под которую ставится слой (мир → манифест → неизвестна). */
    targetMcVersion(state): string | null {
      return targetMcVersionFor(state.pack, state.siteMeta);
    },

    /**
     * Ветка слоя. `null` — слой не нужен или невозможен: карта модовая
     * (свои моды важнее), требование не заявлено или версия MC неизвестна.
     */
    compatBranch(state): CompatKind | null {
      if (!state.pack || state.pack.is_mrpack || state.pack.is_modded || !hasOptifineMarker(state.siteMeta)) {
        return null;
      }
      return compatBranchFor(targetMcVersionFor(state.pack, state.siteMeta));
    },

    /** Метод слоя для плашки на экране цели (точные версии — после резолва). */
    compatTargetMethod(state): string {
      if (!state.compat) return '';
      return state.compat.kind === 'optifine'
        ? `Forge ${state.compat.loader_version} + OptiFine`
        : 'Fabulously Optimized (Fabric)';
    },

    /** Пояснение на экране проверки: зачем нужен OptiFine и что поставим. */
    compatConfirmHint(): string {
      if (!this.requiresOptifine) return '';
      if (this.pack?.is_modded) {
        return 'Карта содержит свои моды — OptiFine не добавляем.';
      }
      const branch = this.compatBranch;
      if (!branch) {
        return 'Карта требует OptiFine, но версию Minecraft определить не удалось — сборка совместимости не будет добавлена.';
      }
      return `Для карты нужен OptiFine. ${compatMethodFor(branch)}.`;
    },

    /** Слой реально уйдёт в деплой: только цель, созданная установщиком. */
    compatInDeploy(state): boolean {
      return !!state.compat && state.targetIsFresh && state.targetModsEmpty;
    },

    compatSkipReason(state): string | null {
      if (!state.compat || !state.target) return null;
      if (!state.targetIsFresh) {
        return 'Сборка совместимости не добавлялась — выбрана существующая версия или инстанс.';
      }
      if (!state.targetModsEmpty) {
        return 'Сборка совместимости не добавлялась: в общей папке уже есть моды, и мы их не заменяем.';
      }
      return null;
    },

    /** Версии для фильтра: офлайн знаем только версию мира из level.dat. */
    filterVersions(): string[] {
      const v = this.targetMcVersion;
      return v ? [v] : [];
    },

    suitableTargets(state): GameTarget[] {
      const pack = state.pack;
      const launcher = state.launcher;
      if (!pack || !launcher) return [];
      const loaderVersion = effectiveLoaderVersion(state);
      const mc = this.targetMcVersion;
      return launcher.targets.filter((t) => {
        if (mc && t.minecraft_version !== mc) return false;
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
      const mc = this.targetMcVersion;
      if (!mc) return null;
      // Кнопка создаёт версию со слоем, а не ванильную: у TLauncher/Legacy
      // папка версии одна на комбинацию, поэтому и сравниваем с той, что
      // реально появится.
      const compat = state.compat;
      const loader = compat?.loader ?? pack.loader;
      const loaderVersion = compat ? compat.loader_version : effectiveLoaderVersion(state);
      return (
        launcher.targets.find(
          (t) =>
            t.minecraft_version === mc &&
            t.loader === loader &&
            (t.loader_version ?? null) === loaderVersion,
        ) ?? null
      );
    },

    /** Подпись и пояснение кнопки «создать» для текущей пары лаунчер/карта. */
    newTargetCta(state): { title: string; hint: string } | null {
      const pack = state.pack;
      const launcher = state.launcher;
      if (!pack || !launcher) return null;
      const mc = this.targetMcVersion ?? '';
      const mapName = state.siteTitle ?? pack.world_name;
      const compat = state.compat;
      const compatLabel = compatPlanLabelFor(compat);

      if (launcher.kind === 'official') {
        return {
          title: compat
            ? `Создать установку «${mapName} ${mc}» · ${compatLabel}`
            : `Создать установку «${mapName} ${mc}»`,
          hint: 'Своя папка игры под карту — ваши установки и миры не пострадают. Таких установок можно сделать сколько угодно, поэтому занятое имя не мешает.',
        };
      }
      if (isVersionLauncher(launcher.kind)) {
        if (!pack.is_modded && !compat) {
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
        const loaderVersion = compat?.loader_version ?? effectiveLoaderVersion(state);
        const loaderName = compat ? LOADER_NAMES[compat.loader] : LOADER_NAMES[pack.loader];
        const version = compat
          ? `${mc} · ${compatLabel}`
          : `${mc} · ${loaderName}${loaderVersion ? ` ${loaderVersion}` : ''}`;
        const hint =
          launcher.kind === 'tlauncher'
            ? compat
              ? 'Подготовим версию с пометкой [KK]. Сборка добавляется только в пустую общую папку модов — существующие моды не заменяем.'
              : 'Скачаем официальный установщик и подготовим версию с пометкой [KK]. У TLauncher папка модов общая для всех версий: замену текущих модов попросим подтвердить, старые сохраним в бэкап.'
            : 'Скачаем официальный установщик и подготовим версию с пометкой [KK] — ничего нажимать не нужно.';
        return { title: `Подготовить «${version}»`, hint };
      }
      return {
        title: compat
          ? `Создать инстанс «${mapName}» · ${compatLabel}`
          : `Создать новый инстанс «${mapName}»`,
        hint: compat
          ? compat.kind === 'optifine'
            ? 'OptiFine ставится с официального сайта, Forge — из официального установщика. Сборка нужна, чтобы карта работала как задумал автор.'
            : 'Сборка нужна, чтобы карта работала без OptiFine: Fabulously Optimized даёт те же возможности на Fabric.'
          : 'Рекомендуется для модовых карт — ничего чужого не тронем.',
      };
    },

    needsWipe(state): boolean {
      return !!(state.pack?.is_modded && state.target && !state.targetModsEmpty);
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
    loadRoots() {
      try {
        this.knownRoots = JSON.parse(localStorage.getItem(KNOWN_ROOTS_KEY) ?? '[]');
      } catch {
        this.knownRoots = [];
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
      // План слоя нужен только текущему флоу: убираем его temp-файлы и
      // запрещаем результату начатого до сброса резолва попасть в новое
      // состояние (иначе всплывёт чужой план, а его файл останется висеть).
      // Ссылку на запрос тоже снимаем: новый флоу должен сделать свой
      // резолв, а не ждать уже брошенный.
      compatGeneration += 1;
      compatRequest = null;
      if (this.compat) void api.discardCompat(this.compat.plan_path).catch(() => {});
      this.compat = null;
      this.compatUnavailable = null;
      this.compatPreparationFailure = null;
      this.compatPrepared = false;
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
      this.targetModsEmpty = false;
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
      this.provisionLog = [];
      this.cancelling = false;
      this.cancelledNotice = null;
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
        this.detection = await api.detectLaunchers(this.knownRoots);
        this.rememberRoots();
        if (this.detection.launchers.length === 0) {
          throw { code: 'E_NO_LAUNCHER', message: 'Лаунчеры не найдены', details: 'no launchers' };
        }
        if (this.detection.launchers.length === 1) {
          this.selectLauncher(this.detection.launchers[0]!);
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
        this.detection = await api.detectLaunchers(this.knownRoots);
        this.rememberRoots();
        if (this.detection.launchers.length === 0) {
          throw { code: 'E_NO_LAUNCHER', message: 'Лаунчеры не найдены', details: 'rescan: none' };
        }
        if (this.detection.launchers.length === 1) {
          this.selectLauncher(this.detection.launchers[0]!);
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
      this.cancelledNotice = null;
      // Экран цели показывает кнопку «подготовить версию» и подходящие версии,
      // а обе вещи зависят от точной версии загрузчика — резолвим её заранее.
      // Провал здесь не фатален: `createVersionTarget` повторит резолв и
      // покажет ту же ошибку, что и раньше.
      void this.ensureLoaderVersion().catch(() => {});
      void this.afterLauncherChosen();
    },

    /**
     * После выбора лаунчера: при необходимости показываем отдельный экран
     * подготовки слоя — от него зависят загрузчик и версия новой цели. Тихое
     * авто-создание ждёт резолв, иначе создался бы ванильный инстанс.
     */
    async afterLauncherChosen() {
      const generation = compatGeneration;
      const willPrepare =
        !this.compatPrepared && this.requiresOptifine && !this.pack?.is_modded && !!this.compatBranch;
      this.step = willPrepare ? 'compat' : 'target';
      await this.prepareCompat();
      if (generation !== compatGeneration) return;
      this.step = 'target';
      if (this.suitableTargets.length === 0) {
        void this.createNewTarget();
      }
    },

    /** Неуспех тоже кэшируется: смена лаунчера не меняет выбранный путь (§7.5). */
    async prepareCompat(): Promise<void> {
      if (this.compatPrepared) return;
      if (compatRequest) {
        await compatRequest;
        return;
      }
      const pack = this.pack;
      if (!pack) return;
      if (!hasOptifineMarker(this.siteMeta) || pack.is_mrpack) {
        this.compatPrepared = true;
        return;
      }
      if (pack.is_modded) {
        this.compatPrepared = true;
        this.compatUnavailable = 'Карта содержит свои моды — OptiFine не добавляем.';
        return;
      }
      const mc = this.targetMcVersion;
      const branch = compatBranchFor(mc);
      if (!mc || !branch) {
        this.compatPrepared = true;
        this.compatUnavailable =
          'Не удалось определить версию Minecraft — сборка совместимости не будет добавлена.';
        return;
      }
      const generation = compatGeneration;
      const request = (compatRequest = (async () => {
        try {
          const summary = await api.prepareCompat(mc, false);
          // Сброс/новый флоу за время запроса: чужой план не должен
          // всплыть, а его temp-файлы надо убрать.
          if (generation !== compatGeneration) {
            void api.discardCompat(summary.plan_path).catch(() => {});
            return;
          }
          this.compat = summary;
          this.compatUnavailable = null;
        } catch (e) {
          if (generation !== compatGeneration) return;
          this.compat = null;
          this.compatPreparationFailure = toCompatFailure(e);
          this.compatUnavailable = compatUnavailableText(e, branch);
        }
      })());
      try {
        await request;
      } finally {
        // Ссылку снимаем, только если запрос всё ещё наш: после `reset()`
        // мог начаться новый — иначе старый finally обнулил бы его.
        if (compatRequest === request) compatRequest = null;
        if (generation === compatGeneration) {
          this.compatPrepared = true;
        }
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
      const mc = this.targetMcVersion;
      if (!mc) return null;
      if (this.loaderVersion) return this.loaderVersion;
      // Запрос в полёте переиспользуем: выбрать лаунчер с авто-созданием цели
      // и нажать кнопку можно не дожидаясь сети, а второй запрос вернул бы то
      // же самое. Сбрасываем в любом исходе — после провала клик должен суметь
      // повторить.
      loaderVersionRequest ??= (async () => {
        // Для `.mrpack` индекс автора задаёт точную версию; для обычного zip
        // приоритет у точной версии сайта, затем хинт архива → meta-API.
        const siteVersion = pack.is_mrpack ? null : siteLoaderVersionHint(this.siteMeta);
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
     * дополняют друг друга, а не затирают).
     */
    async beginInstallWork() {
      const generation = compatGeneration;
      const launcher = this.launcher;
      if (!launcher) return;
      await launcherReturnRequest;
      if (generation !== compatGeneration || this.launcher !== launcher) return;
      const request = (launcherCloseRequest ??= api.closeLauncher(launcher.kind).catch(() => false));
      try {
        const wasRunning = await request;
        if (generation === compatGeneration && this.launcher === launcher) {
          this.launcherWasRunning ||= wasRunning;
        }
      } finally {
        if (launcherCloseRequest === request) launcherCloseRequest = null;
      }
    },

    /**
     * Возвращает лаунчер, если мы его закрывали (и на успехе, и на провале).
     * Молча: установка уже завершена, шум ни к чему. Флаг сбрасываем сразу,
     * чтобы повторный вызов не открыл его дважды.
     */
    async endInstallWork() {
      const launcher = this.launcher;
      const closing = launcherCloseRequest;
      const wasRunning = this.launcherWasRunning;
      if (!launcher || (!closing && !wasRunning)) {
        await launcherReturnRequest;
        return;
      }
      launcherCloseRequest = null;
      this.launcherWasRunning = false;
      const request = (launcherReturnRequest = (async () => {
        const closed = closing ? await closing : false;
        if (wasRunning || closed) {
          await api.reopenLauncher(launcher.kind, launcher.exe_path).catch(() => {});
        }
      })());
      try {
        await request;
      } finally {
        if (launcherReturnRequest === request) launcherReturnRequest = null;
      }
    },

    async createNewTarget() {
      const pack = this.pack;
      const launcher = this.launcher;
      if (!pack || !launcher) return;
      // У TLauncher, Legacy Launcher и официального лаунчера цели — версии,
      // а не инстансы: их готовит отдельная команда, `create_instance` не
      // вызывается.
      if (isVersionLauncher(launcher.kind)) {
        await this.createVersionTarget();
        return;
      }
      const mc = this.targetMcVersion;
      if (!mc) {
        this.fail(
          {
            code: 'E_UNKNOWN_LOADER',
            message: 'Неизвестна версия игры',
            details: 'world_version and manifest.minecraft.primary are null',
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
        // Слой задаёт загрузчик и его точную версию (FO пинит fabric-loader,
        // OptiFine — версию Forge со страницы). Без слоя — прежний путь.
        const compat = this.compat;
        let loaderVersion: string | null = null;
        let auto = false;
        if (compat) {
          loaderVersion = compat.loader_version;
        } else if (pack.is_modded) {
          // Индекс `.mrpack` важнее метаданных сайта; для обычного zip точная
          // версия сайта идёт первой, затем хинт архива → meta-API.
          const siteVersion = pack.is_mrpack ? null : siteLoaderVersionHint(this.siteMeta);
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
          loader: compat?.loader ?? pack.loader,
          loaderVersion,
          iconPath: this.siteIconPath,
        });
        this.targetIsFresh = true;
        this.targetModsEmpty = true;
        // Обновляем детект, чтобы новый инстанс был виден везде.
        this.detection = await api.detectLaunchers(this.knownRoots);
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
      this.targetModsEmpty = false;
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
     * пометка [KK] → предвыбор. Происхождение версии и пустота общих модов
     * проверяются отдельно: создание версии не разрешает заменить чужие моды.
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

      const mc = this.targetMcVersion;
      const compat = this.compat;
      // TLauncher — единственный из «без инстансов», кому ванилле версия не
      // нужна: у него одна папка игры на все версии. Legacy раскладывает
      // saves по подпапкам режима, поэтому даже ванильной карте нужна
      // подготовка — указать папку `home/<версия>` и предвыбрать версию.
      // Официальному лаунчеру нужна своя установка. Слой совместимости
      // требует загрузчика, поэтому версию готовим и для TLauncher.
      const needsVersion = pack.is_modded || !!compat || kind === 'official' || kind === 'legacy';
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
        this.targetIsFresh = false;
        this.targetModsEmpty = false;
        await this.proceedAfterTarget();
        return;
      }
      if (!mc) {
        this.fail(
          {
            code: 'E_UNKNOWN_LOADER',
            message: 'Неизвестна версия игры',
            details: 'world_version and manifest.minecraft.primary are null',
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
      this.provisionLog = [];
      this.cancelling = false;
      const unlisten = await listen<StageEvent>('provision-progress', (event) => {
        this.progress = event.payload;
        const line = event.payload.detail;
        if (line) {
          this.provisionLog.push(line);
          // Лог живой и может расти на сотни строк: держим хвост, чтобы
          // память и рендер не росли вместе с установкой.
          if (this.provisionLog.length > PROVISION_LOG_MAX) {
            this.provisionLog.splice(0, this.provisionLog.length - PROVISION_LOG_MAX);
          }
        }
      });
      try {
        // Слой задаёт загрузчик и точную версию; иначе — загрузчик карты.
        const loader = compat?.loader ?? pack.loader;
        const loaderVersion = compat
          ? compat.loader_version
          : await this.ensureLoaderVersion();
        this.statusLine =
          loader !== 'vanilla'
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
            loader,
            loaderVersion,
            iconPath: this.siteIconPath,
          });
        } else if (kind === 'legacy') {
          res = await api.ensureLegacyVersion({
            installDir: launcher.exe_path ?? launcher.data_dir,
            mcVersion: mc,
            loader,
            loaderVersion,
          });
        } else {
          res = await api.ensureTlauncherVersion({
            dataDir: launcher.data_dir,
            mcVersion: mc,
            loader,
            loaderVersion,
          });
        }
        this.target = res.target;
        this.targetIsFresh = res.created;
        this.targetModsEmpty = res.mods_empty;
        if (!res.preselected) {
          this.versionNote = `${LAUNCHER_NAMES[kind]} не удалось закрыть — выберите версию «${res.target.name}» в лаунчере вручную.`;
        }
        // Обновляем детект, чтобы подготовленная версия была видна везде.
        this.detection = await api.detectLaunchers(this.knownRoots);
        this.rememberRoots();
        this.launcher =
          this.detection.launchers.find(
            (l) => l.kind === kind && l.data_dir === launcher.data_dir,
          ) ?? launcher;
        await this.proceedAfterTarget();
      } catch (e) {
        await this.endInstallWork();
        if (isCancelError(e)) {
          this.finishCancel(wasStep);
          return;
        }
        this.fail(e, wasStep === 'deploying' ? 'target' : wasStep);
      } finally {
        unlisten();
      }
    },

    /**
     * Отмена по воле пользователя: возвращаем к выбору лаунчера с тихой
     * заметкой. Красный экран ошибки здесь врал бы — установка прервана
     * намеренно, следов не осталось (бэкенд убрал докачанное и снёс процессы).
     */
    finishCancel(returnStep: Step) {
      this.cancelling = false;
      this.busy = false;
      this.progress = null;
      this.statusLine = '';
      this.step = returnStep === 'deploying' ? 'launcher' : returnStep;
      this.cancelledNotice =
        'Установка отменена. Докачанные файлы убраны — следующая попытка начнётся заново.';
    },

    /** Кнопка «Отмена»: ставим флаг и ждём `E_CANCELLED` из висящего вызова. */
    async cancelProvisioning() {
      if (this.cancelling) return;
      this.cancelling = true;
      await api.cancelProvision().catch(() => {
        // Флаг не дошёл — снимаем ожидание, установка продолжится как обычно.
        this.cancelling = false;
      });
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
        // Слой — только в цель, созданную установщиком: существующий инстанс
        // получает лишь карту (явное правило, без исключений).
        const compat = this.compatInDeploy ? this.compat : null;
        this.report = await api.deployPack(
          target,
          {
            zip_path: this.zipPath,
            world_prefix: pack.world_prefix,
            content_prefix: pack.content_prefix,
            overrides_alias: pack.overrides_alias,
            world_name: this.finalWorldName || pack.world_name,
            layers: pack.layers,
            deploy_mods: pack.is_modded,
            wipe_mods: this.wipeConfirmed,
            resource_packs: this.selectedResourcePacks(),
            compat_plan_path: compat?.plan_path ?? null,
          },
          compat,
        );
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
     * Повтор слоя в уже созданной цели: мир не переустанавливается, поэтому
     * команда отдельная (`apply_compat`). План в temp живёт до ухода с
     * «Готово»/сброса, так что резолв заново не нужен.
     */
    async retryCompat() {
      const target = this.target;
      const compat = this.compat;
      const installReport = this.report;
      if (!target || !compat || !installReport || !this.compatInDeploy || this.busy) return;
      const generation = compatGeneration;
      const isCurrent = () =>
        generation === compatGeneration &&
        this.target === target &&
        this.compat === compat &&
        this.report === installReport;
      this.busy = true;
      this.statusLine = 'Ставим сборку совместимости…';
      try {
        await this.beginInstallWork();
        if (!isCurrent()) return;
        const report = await api.applyCompat(target, compat, (event) => {
          if (isCurrent()) this.progress = event;
        });
        if (isCurrent()) installReport.compat = report;
      } catch (e) {
        // Non-fatal: карта уже стоит, показываем причину на «Готово».
        if (isCurrent()) {
          installReport.compat = {
            kind: compat.kind,
            display_name: compat.display_name,
            version: compat.version,
            files_written: 0,
            jar_sha256: null,
            error: toCompatFailure(e),
          };
        }
      } finally {
        if (isCurrent()) {
          this.statusLine = '';
          await this.endInstallWork();
          if (isCurrent()) this.busy = false;
        }
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
          });
        } else if (target.launcher === 'modrinth') {
          await api.launchGame({
            launcher: 'modrinth',
            exe,
            instanceId: target.path,
            world: null,
          });
        } else {
          // Для TLauncher/Legacy `instanceId` — имя версии (её выбирает CLI
          // лаунчера), для официального — что угодно: запускается приложение.
          await api.launchGame({
            launcher: target.launcher,
            exe,
            instanceId: target.name,
            world: null,
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
