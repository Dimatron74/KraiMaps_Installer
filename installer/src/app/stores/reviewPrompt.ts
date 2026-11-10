/**
 * Рекомендация оценить установленную карту. Журнал установок и отметки
 * «уже рекомендовали» — в localStorage: переживают перезапуск, не требуют
 * сети и не содержат секретов.
 */
import { defineStore } from 'pinia';
import { KRAIMAPS_SITE_URL } from '../../shared/config';
import {
  buildMapUrl,
  emptyReviewState,
  markRecommended,
  parseReviewState,
  selectReviewCandidate,
  upsertInstall,
  type InstalledMap,
  type ReviewPromptState,
  type ReviewTrigger,
} from '../../shared/utils/reviewPrompt';
import { useNotificationsStore } from './notifications';

const STORAGE_KEY = 'kraimaps.review-prompt.v1';

function readStorage(): ReviewPromptState {
  try {
    return parseReviewState(localStorage.getItem(STORAGE_KEY));
  } catch {
    return emptyReviewState();
  }
}

function writeStorage(state: ReviewPromptState): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // Квота или приватный режим: рекомендация не критична, установку не ломаем.
  }
}

export const useReviewPromptStore = defineStore('review-prompt', {
  state: () => ({
    installs: [] as InstalledMap[],
    recommended: [] as number[],
    lastShownAt: 0,
    loaded: false,
    open: false,
    duringInstall: false,
    candidate: null as InstalledMap | null,
  }),

  actions: {
    load() {
      if (this.loaded) return;
      const state = readStorage();
      this.installs = state.installs;
      this.recommended = state.recommended;
      this.lastShownAt = state.lastShownAt;
      this.loaded = true;
    },

    snapshot(): ReviewPromptState {
      return {
        installs: this.installs,
        recommended: this.recommended,
        lastShownAt: this.lastShownAt,
      };
    },

    persist() {
      writeStorage(this.snapshot());
    },

    /** Деплой фиксирует историю независимо от посещения экрана «Готово» и телеметрии. */
    recordCompletedInstall(mapId: number, title: string) {
      this.load();
      const next = upsertInstall(this.snapshot(), {
        mapId,
        title: title.trim() || `Карта №${mapId}`,
        installedAt: Date.now(),
      });
      this.installs = next.installs;
      this.persist();
    },

    /**
     * Показать рекомендацию, если есть подходящая карта и 12 часов прошли.
     * Отметку «уже рекомендовали» ставит `markShown` — только после того,
     * как модалка действительно отрисовалась.
     * Возвращает `false`, если модалку показывать не нужно.
     */
    maybeShow(trigger: ReviewTrigger): boolean {
      this.load();
      if (this.open) return false;

      const candidate = selectReviewCandidate(this.snapshot(), Date.now());
      if (!candidate) return false;

      this.candidate = candidate;
      this.duringInstall = trigger === 'install';
      this.open = true;
      return true;
    },

    /** Вызывать после фактического показа модалки (её `onMounted`). */
    markShown() {
      if (!this.open || !this.candidate) return;

      const now = Date.now();
      const next = markRecommended(this.snapshot(), this.candidate.mapId, now);
      this.recommended = next.recommended;
      this.lastShownAt = next.lastShownAt;
      this.persist();
    },

    dismiss() {
      this.open = false;
      this.candidate = null;
      this.duringInstall = false;
    },

    async rate() {
      const candidate = this.candidate;
      this.dismiss();
      if (!candidate) return;

      const url = buildMapUrl(KRAIMAPS_SITE_URL, candidate.mapId);
      try {
        const { openUrl } = await import('@tauri-apps/plugin-opener');
        await openUrl(url);
      } catch {
        useNotificationsStore().error(
          'Не удалось открыть браузер',
          `Откройте страницу карты вручную: ${url}`,
        );
      }
    },
  },
});
