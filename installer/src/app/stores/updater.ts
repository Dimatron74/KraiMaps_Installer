/**
 * Стор обновлений установщика (Pinia).
 *
 * Проверка — тихая при старте (`checkAuto`, fail-open: офлайн молчим и
 * работаем дальше) и ручная (`checkManual`). Политика `soft`/`hard`
 * решается Rust-командой `updater_check`; стор только показывает:
 * `soft` — оффер с отказом, `hard` — полноэкранный блок без обхода
 * (запасной выход — ручное скачивание со страницы `/installer`).
 * Блокировка — только по свежему явному сигналу сервера в этой сессии:
 * закэшированных блокировок нет, чтобы outage не кирпичил приложение.
 */
import { defineStore } from 'pinia';
import { api } from '../../shared/api/commands';
import { INSTALLER_VERSION_FALLBACK, KRAIMAPS_SITE_URL } from '../../shared/config';
import { toErrorCode, toErrorDetails, toUserMessage } from '../../shared/utils/errors';
import type { UpdateDownloadEvent, UpdateStatusName } from '../../shared/types/updater';

const AUTO_KEY = 'mi.updates.autoCheck';
const DISMISSED_KEY = 'mi.updates.dismissedFor';

export type UpdatePhase = 'idle' | 'checking' | 'downloading' | 'ready' | 'error';

export interface UpdateError {
  code: string;
  message: string;
  details: string;
}

function readAutoCheck(): boolean {
  try {
    const raw = localStorage.getItem(AUTO_KEY);
    return raw === null ? true : raw === '1';
  } catch {
    return true;
  }
}

export const useUpdaterStore = defineStore('updater', {
  state: () => ({
    /** Текущая версия приложения (Rust, fallback — package.json для pnpm dev). */
    version: INSTALLER_VERSION_FALLBACK as string,
    /** Результат последней проверки: до первой — `unknown` (ничего не показываем). */
    status: 'unknown' as 'unknown' | UpdateStatusName,
    phase: 'idle' as UpdatePhase,
    latest: null as string | null,
    minSupported: null as string | null,
    notes: null as string | null,
    /** Видимость мягкого оффера (hard показывается отдельным экраном). */
    offerOpen: false,
    /** Прогресс установки 0..100 (`null` — неизвестен размер). */
    percent: null as number | null,
    error: null as UpdateError | null,
    /** Итоговое сообщение ручной проверки («последняя версия» / рестарт). */
    manualMessage: null as string | null,
    /** Тихая автопроверка при старте (настройка; UI экрана пока нет — флаг на будущее). */
    autoCheck: readAutoCheck(),
  }),

  getters: {
    /** Полноэкранный блок: только свежий `hard` из этой сессии. */
    blocked: (s) => s.status === 'hard',
    /**
     * Идёт ли установка прямо сейчас. Отдельный геттер, а не сравнение
     * `this.phase` по месту: фаза — разделяемое мутабельное состояние,
     * после `await` её надо читать заново, а TS сужает тип геттер
     * не трогает (граница функции).
     */
    downloading: (s) => s.phase === 'downloading',
  },

  actions: {
    async loadVersion(): Promise<void> {
      try {
        this.version = await api.getInstallerVersion();
      } catch {
        this.version = INSTALLER_VERSION_FALLBACK;
      }
    },

    setAutoCheck(on: boolean): void {
      this.autoCheck = on;
      try {
        localStorage.setItem(AUTO_KEY, on ? '1' : '0');
      } catch {
        /* приватный режим — настройка на сессию */
      }
    },

    /** Тихая проверка при старте. Никогда не бросает исключение. */
    async checkAuto(): Promise<void> {
      if (!this.autoCheck || this.phase === 'downloading' || this.status === 'hard') return;
      try {
        await this.runCheck(false);
      } catch {
        /* fail-open: молчим, работаем дальше */
      }
    },

    /** Ручная проверка (кнопка). Ошибки — видимым состоянием, не молча. */
    async checkManual(): Promise<void> {
      // Кнопка в это время задизейблена с текстом «Проверяем…», но вызов
      // может прийти и программно: отвечаем сообщением, а не тишиной.
      if (this.phase === 'checking' || this.phase === 'downloading') {
        this.manualMessage = 'Проверка уже идёт…';
        return;
      }
      this.manualMessage = null;
      await this.runCheck(true);
    },

    dismissOffer(): void {
      this.offerOpen = false;
      try {
        if (this.latest) localStorage.setItem(DISMISSED_KEY, this.latest);
      } catch {
        /* best-effort */
      }
    },

    /** Скачивание и установка. Прогресс — из канала Rust. */
    async startUpdate(): Promise<void> {
      if (this.phase === 'downloading') return;
      this.phase = 'downloading';
      this.percent = null;
      this.error = null;
      this.manualMessage = null;
      let downloaded = 0;
      let total: number | null = null;
      const onEvent = (ev: UpdateDownloadEvent) => {
        if (ev.event === 'Started') {
          total = ev.data.contentLength;
        } else if (ev.event === 'Progress') {
          downloaded += ev.data.chunkLength;
          this.percent = total && total > 0 ? Math.min(100, (downloaded / total) * 100) : null;
        } else if (ev.event === 'Finished') {
          // Скачано, идёт установка (на Windows приложение сейчас завершится само).
          this.percent = 100;
        } else {
          // Неизвестный будущий вариант протокола: игнорируем, а не
          // притворяемся «скачано». Исчерпываемость проверяет компилятор:
          const _exhaustive: never = ev;
          void _exhaustive;
        }
      };
      try {
        await api.updaterInstall(onEvent);
      } catch (e) {
        this.phase = 'error';
        this.error = { code: toErrorCode(e), message: toUserMessage(e), details: toErrorDetails(e) };
        return;
      }
      // Установка инициирована: просим рантайм перезапуститься на новую версию.
      // В браузере (pnpm dev) и при авто-выходе Windows это недоступно/не нужно.
      try {
        const { relaunch } = await import('@tauri-apps/plugin-process');
        await relaunch();
      } catch {
        this.phase = 'ready';
        this.manualMessage = 'Обновление установлено. Перезапустите приложение вручную.';
      }
    },

    /** Запасной выход из hard-блока: ручное скачивание со страницы сайта. */
    async openManualDownload(): Promise<void> {
      try {
        const { openUrl } = await import('@tauri-apps/plugin-opener');
        await openUrl(`${KRAIMAPS_SITE_URL}/installer`);
      } catch {
        this.error = {
          code: 'E_UNKNOWN',
          message: 'Не удалось открыть страницу скачивания',
          details: `${KRAIMAPS_SITE_URL}/installer`,
        };
      }
    },

    async runCheck(manual: boolean): Promise<void> {
      // Зависшая проверка не смеет трогать живое скачивание: если за время
      // await пользователь успел нажать «Обновить», прогресс важнее.
      if (this.downloading) return;
      this.phase = 'checking';
      this.error = null;
      let result;
      try {
        result = await api.updaterCheck();
      } catch (e) {
        if (this.downloading) return;
        const code = toErrorCode(e);
        // Тихая проверка не оставляет следов: ни офлайн, ни 5xx, ни битый
        // JSON не складываем в error — иначе скрытое состояние всплывёт
        // в следующем оффере/блоке как якобы актуальное. Видит ошибки
        // только ручная проверка.
        this.phase = 'idle';
        if (manual) {
          this.error = { code, message: toUserMessage(e), details: toErrorDetails(e) };
        }
        return;
      }
      // Проверка вернулась уже после старта установки — результат устарел,
      // состояние скачивания не трогаем.
      if (this.downloading) return;
      this.status = result.status;
      this.latest = result.latest;
      this.minSupported = result.minSupported;
      this.notes = result.notes;
      this.phase = 'idle';
      if (result.status === 'soft') {
        let dismissed: string | null = null;
        try {
          dismissed = localStorage.getItem(DISMISSED_KEY);
        } catch {
          /* читаем как есть */
        }
        // Ручная проверка показывает оффер всегда; авто — если версию ещё не откладывали.
        this.offerOpen = manual || dismissed !== result.latest;
      } else {
        this.offerOpen = false;
        if (result.status === 'current' && manual) {
          this.manualMessage = 'У вас последняя версия.';
        }
      }
    },
  },
});
