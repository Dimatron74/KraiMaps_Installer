/**
 * Глобальные UI-уведомления (toast) — перенос сайтового
 * `useGlobalNotification` на Pinia. Очередь общая на приложение, а таймеры
 * автоскрытия живут здесь, а не в компоненте: плашка не должна исчезать
 * вместе с экраном, который её показал.
 */
import { defineStore } from 'pinia';
import type {
  NotificationAction,
  UiNotification,
  UiNotificationType,
} from '../../shared/types/notifications';

const DEFAULT_DURATION = 5000;
/** Четвёртая плашка уже перекрывает заголовок окна 720 px. */
const MAX_VISIBLE = 3;

const timers = new Map<string, number>();
let fallbackSeq = 0;

function generateId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  fallbackSeq += 1;
  return `notif-${Date.now()}-${fallbackSeq.toString(36)}`;
}

export const useNotificationsStore = defineStore('notifications', {
  state: () => ({
    items: [] as UiNotification[],
  }),

  actions: {
    show(
      type: UiNotificationType,
      title: string,
      message?: string,
      duration: number = DEFAULT_DURATION,
      action?: NotificationAction,
    ): string {
      const id = generateId();
      while (this.items.length >= MAX_VISIBLE) {
        const oldest = this.items[0];
        if (!oldest) break;
        this.remove(oldest.id);
      }

      const item: UiNotification = { id, type, title };
      if (message) item.message = message;
      if (action) item.action = action;
      this.items.push(item);

      if (duration > 0) {
        timers.set(
          id,
          window.setTimeout(() => this.remove(id), duration),
        );
      }
      return id;
    },

    remove(id: string) {
      const timer = timers.get(id);
      if (timer !== undefined) {
        window.clearTimeout(timer);
        timers.delete(id);
      }
      const index = this.items.findIndex((n) => n.id === id);
      if (index !== -1) this.items.splice(index, 1);
    },

    success(title: string, message?: string, duration?: number, action?: NotificationAction) {
      return this.show('success', title, message, duration, action);
    },

    error(title: string, message?: string, duration?: number, action?: NotificationAction) {
      return this.show('error', title, message, duration, action);
    },

    warning(title: string, message?: string, duration?: number, action?: NotificationAction) {
      return this.show('warning', title, message, duration, action);
    },

    info(title: string, message?: string, duration?: number, action?: NotificationAction) {
      return this.show('info', title, message, duration, action);
    },
  },
});
