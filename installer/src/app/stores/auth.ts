/**
 * Auth-стор: только состояние сессии и профиль для шапки.
 * Токены живут исключительно в Rust/keyring, в JS/localStorage их нет.
 */
import { defineStore } from 'pinia';
import { api } from '../../shared/api/commands';
import {
  isSessionDeadCode,
  toErrorCode,
  toErrorDetails,
  toUserMessage,
} from '../../shared/utils/errors';
import type { SiteUser } from '../../shared/types/site';

export type AuthStatus = 'unknown' | 'guest' | 'authorizing' | 'authed';

export const useAuthStore = defineStore('auth', {
  state: () => ({
    status: 'unknown' as AuthStatus,
    user: null as SiteUser | null,
    /** mapId из kraimaps://install/{id}, ждёт входа. */
    pendingMapId: null as number | null,
    pendingMode: 'auto' as string,
    error: null as string | null,
    /** Техдетали для «Подробнее» (код E_* + контекст Rust). */
    errorCode: null as string | null,
    errorDetails: null as string | null,
    /**
     * Транзиентный сбой связи с сайтом (сеть, 503, 429 — I-3).
     * Пока выставлен — статус и пользователя сохраняем, гостем НЕ объявляем;
     * UI показывает «нет соединения» с кнопкой повтора.
     */
    transientError: null as string | null,
    transientCode: null as string | null,
    transientDetails: null as string | null,
    /**
     * Переживёт ли сессия перезапуск (I-4.2). `false` — хранилище недоступно
     * (на Linux обычно нет Secret Service): показываем предупреждение,
     * а не тихий гость. `null` — ещё не проверяли.
     */
    storePersistent: null as boolean | null,
  }),

  getters: {
    isAuthed(state): boolean {
      return state.status === 'authed';
    },
    /** Есть несообщённый транзиентный сбой — нужен баннер «нет соединения». */
    hasConnectionIssue(state): boolean {
      return state.transientError !== null;
    },
  },

  actions: {
    setError(e: unknown) {
      this.error = toUserMessage(e);
      this.errorCode = toErrorCode(e);
      this.errorDetails = toErrorDetails(e);
    },

    clearError() {
      this.error = null;
      this.errorCode = null;
      this.errorDetails = null;
    },

    setTransient(e: unknown) {
      this.transientError = toUserMessage(e);
      this.transientCode = toErrorCode(e);
      this.transientDetails = toErrorDetails(e);
    },

    clearTransient() {
      this.transientError = null;
      this.transientCode = null;
      this.transientDetails = null;
    },

    /**
     * Проверка хранилища сессии (I-4.2). Best-effort: неуспех проверки —
     * тоже транзиент, сессию не трогаем.
     */
    async checkStore() {
      try {
        const st = await api.authStoreStatus();
        this.storePersistent = st.persistent;
      } catch {
        /* хранилище неизвестно — молчим, это не разлогин */
      }
    },

    /** Повтор после транзиентного сбоя: сбрасываем флаг и перечитываем профиль. */
    async retry() {
      this.clearTransient();
      await this.refresh();
    },

    async refresh() {
      try {
        const me = await api.authProfile();
        if (me.is_authenticated && me.user) {
          this.user = me.user;
          this.status = 'authed';
        } else {
          // Rust возвращает гостя только при доказанной смерти сессии
          // (мёртвый refresh с 401) — это честный гость. Заодно проверяем
          // хранилище: если оно сломано (Linux без Secret Service), покажем
          // предупреждение вместо тихого гостя после каждого рестарта (I-4).
          this.user = null;
          this.status = 'guest';
          await this.checkStore();
        }
        this.clearError();
        this.clearTransient();
      } catch (e) {
        if (isSessionDeadCode(toErrorCode(e))) {
          // Оборонительная ветка: по контракту Rust мёртвая сессия идёт через
          // Ok(guest), но если 401 просочился исключением — сессия мертва.
          this.user = null;
          this.status = 'guest';
          this.clearTransient();
          this.setError(e);
        } else {
          // Транзиент (I-3): статус и пользователя сохраняем, гостем
          // НЕ объявляем. Ретрай вернёт всё сам при появлении сети.
          this.setTransient(e);
          // Заодно проверяем хранилище: если сессии нет из-за незаписи
          // в keyring — покажем предупреждение вместо тихого гостя (I-4).
          await this.checkStore();
        }
      }
    },

    /** Начать вход: Rust готовит PKCE и URL, фронт открывает системный браузер. */
    async startLogin(mapId: number | null = null) {
      const prevStatus = this.status;
      const hadUser = this.user !== null;
      this.status = 'authorizing';
      this.clearError();
      if (mapId !== null) this.pendingMapId = mapId;
      try {
        const { url } = await api.oauthStart();
        const { openUrl } = await import('@tauri-apps/plugin-opener');
        await openUrl(url);
      } catch (e) {
        // Не убиваем живую сессию из-за локального сбоя (не открылся браузер):
        // откатываемся к предыдущему статусу, а не в гостя.
        if (hadUser && prevStatus === 'authed') {
          this.status = 'authed';
        } else {
          this.status = 'guest';
        }
        this.setError(e);
        throw e;
      }
    },

    /** Deep-link kraimaps://oauth/callback?code=..&state=.. — обмен в Rust. */
    async handleOAuthCallback(code: string, state: string) {
      try {
        await api.oauthExchange(code, state);
        await this.refresh();
      } catch (e) {
        // Обмен мог упасть на протухшем/повторном коде при живой сессии —
        // перепроверяем профиль, прежде чем объявлять «гостя».
        await this.refresh();
        if (this.isAuthed) return;
        // Сеть легла посреди входа — не гость: статус сохранён, ждём retry (I-3).
        if (this.hasConnectionIssue) throw e;
        this.status = 'guest';
        this.setError(e);
        throw e;
      }
    },

    async logout() {
      try {
        await api.authLogout();
      } finally {
        this.user = null;
        this.status = 'guest';
        this.pendingMapId = null;
        this.clearTransient();
      }
    },

    /** «Установить без авторизации»: запоминаем карту как подсказку и уходим на главную. */
    continueWithoutAuth(mapId: number | null = null) {
      this.pendingMapId = mapId;
      this.status = 'guest';
    },
  },
});
