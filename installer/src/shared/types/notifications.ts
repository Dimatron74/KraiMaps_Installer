/** Типы глобальных UI-уведомлений установщика (toast), аналог сайтовых. */

export type UiNotificationType = 'success' | 'error' | 'warning' | 'info';

export interface NotificationAction {
  label: string;
  onClick: () => void | Promise<void>;
}

export interface UiNotification {
  id: string;
  type: UiNotificationType;
  title: string;
  message?: string;
  action?: NotificationAction;
}
