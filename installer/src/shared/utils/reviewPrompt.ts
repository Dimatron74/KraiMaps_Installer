/**
 * Чистая логика рекомендации оценить карту: без Vue, DOM и Tauri.
 * Хранилище живёт в `app/stores/reviewPrompt.ts`, чтобы правила выбора
 * можно было проверить тестом без рантайма приложения.
 */

/** Карта считается «поигранной» через сутки после установки. */
export const REVIEW_MIN_AGE_MS = 24 * 60 * 60 * 1000;
/** Между показами рекомендаций — не меньше 12 часов. */
export const REVIEW_INTERVAL_MS = 12 * 60 * 60 * 1000;

export interface InstalledMap {
  mapId: number;
  title: string;
  /** Момент успешного завершения установки, мс. */
  installedAt: number;
}

export interface ReviewPromptState {
  installs: InstalledMap[];
  recommended: number[];
  /** 0 — рекомендаций ещё не было. */
  lastShownAt: number;
}

export type ReviewTrigger = 'home' | 'install';

interface ReviewContext {
  startupReady: boolean;
  page: string | symbol | null | undefined;
  installStep: string;
  siteStep: string;
  blocked: boolean;
}

export function emptyReviewState(): ReviewPromptState {
  return { installs: [], recommended: [], lastShownAt: 0 };
}

/** Адрес страницы карты: сайт принимает id без slug. */
export function buildMapUrl(siteBase: string, mapId: number): string {
  return `${siteBase.replace(/\/+$/, '')}/maps/${mapId}`;
}

/** Не показываем модалку на старте deep-link: только когда деплой уже начат. */
export function isReviewInstallStep(step: string): boolean {
  return step === 'deploying';
}

export function getReviewTrigger(context: ReviewContext): ReviewTrigger | null {
  if (!context.startupReady || context.blocked) return null;
  if (context.page === 'home') return 'home';
  if (
    context.page === 'install' &&
    context.siteStep === 'idle' &&
    isReviewInstallStep(context.installStep)
  ) {
    return 'install';
  }
  return null;
}

/** Самая давняя установка среди подходящих; `null` — рекомендовать нечего. */
export function selectReviewCandidate(
  state: ReviewPromptState,
  now: number = Date.now(),
): InstalledMap | null {
  if (state.lastShownAt > 0 && now - state.lastShownAt < REVIEW_INTERVAL_MS) return null;

  const alreadyRecommended = new Set(state.recommended);
  let oldest: InstalledMap | null = null;

  for (const item of state.installs) {
    if (alreadyRecommended.has(item.mapId)) continue;
    if (now - item.installedAt < REVIEW_MIN_AGE_MS) continue;
    if (!oldest || item.installedAt < oldest.installedAt) oldest = item;
  }

  return oldest;
}

/** Повторная установка той же карты обновляет время, а не создаёт дубль. */
export function upsertInstall(state: ReviewPromptState, next: InstalledMap): ReviewPromptState {
  const installs = state.installs.filter((item) => item.mapId !== next.mapId);
  installs.push(next);
  installs.sort((a, b) => a.installedAt - b.installedAt);
  return { ...state, installs };
}

/** Отметка ставится в момент показа: повторно эту карту не рекомендуем. */
export function markRecommended(
  state: ReviewPromptState,
  mapId: number,
  now: number,
): ReviewPromptState {
  const recommended = state.recommended.includes(mapId)
    ? state.recommended
    : [...state.recommended, mapId];
  return {
    ...state,
    recommended,
    lastShownAt: now,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isPositiveInt(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value > 0;
}

/** Битая запись в localStorage не должна ломать приложение — просто сбрасываем. */
export function parseReviewState(raw: string | null): ReviewPromptState {
  if (!raw) return emptyReviewState();

  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return emptyReviewState();
  }
  if (!isRecord(data)) return emptyReviewState();

  const installs: InstalledMap[] = [];
  const seen = new Set<number>();
  if (Array.isArray(data.installs)) {
    for (const entry of data.installs) {
      if (!isRecord(entry)) continue;
      const { mapId, title, installedAt } = entry;
      if (!isPositiveInt(mapId) || seen.has(mapId)) continue;
      if (typeof title !== 'string' || !title.trim()) continue;
      if (typeof installedAt !== 'number' || !Number.isFinite(installedAt) || installedAt <= 0) {
        continue;
      }
      seen.add(mapId);
      installs.push({
        mapId,
        title: title.trim(),
        installedAt,
      });
    }
  }

  const recommended = Array.isArray(data.recommended)
    ? [...new Set(data.recommended.filter(isPositiveInt))]
    : [];
  const lastShownAt =
    typeof data.lastShownAt === 'number' &&
    Number.isFinite(data.lastShownAt) &&
    data.lastShownAt > 0
      ? data.lastShownAt
      : 0;

  return {
    installs,
    recommended,
    lastShownAt,
  };
}
